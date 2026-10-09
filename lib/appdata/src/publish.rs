//! A closed registry kept in the application's own store, and a settings
//! surface kept responsive while the store is written.
//!
//! - **An edit changes the live record, not the store.** Writing per change
//!   would cost a service round trip and a disk commit per pointer-motion
//!   sample of a drag, so the write is asked for where the interaction
//!   settles.
//! - **The store is written off the loop, and its answer is what applies** —
//!   to every setting the user has not changed since that write was asked
//!   for. A machine policy or a restore wins wherever the user is not
//!   editing, and an answer never moves a control the user is dragging.
//! - **One write is outstanding at a time.** What is asked for meanwhile is
//!   owed until the answer lands, so every answer describes the only write in
//!   flight, a restore cannot be displaced by a later save, and no save
//!   writes back values a restore is about to remove.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use tairix_abi::Errno;
use tairix_appconf::{Keys, Live, Registry};

use crate::Settings;

/// Write `record` into `store` and commit, touching only what the store's
/// layers do not already imply: a value a machine policy or the bundle's
/// defaults supply is never copied up into the user's own document, and a
/// stored value the registry refuses is replaced.
///
/// # Errors
///
/// The service's refusal of the commit, or [`Errno::OutOfRange`] for a
/// spelling the format refuses, which is a defect of the registry.
pub fn save<R: Registry>(record: &R, store: &mut Settings<'_>) -> Result<(), Errno> {
    let (stored, refused) = R::load(store);
    let (mut held, mut wanted) = (String::new(), String::new());
    for &key in R::KEYS {
        held.clear();
        wanted.clear();
        let had = stored.spell(key, &mut held);
        let has = record.spell(key, &mut wanted);
        if had == has && held == wanted && !refused.contains(&key) {
            continue;
        }
        if has {
            store
                .set(R::name(key), &wanted)
                .map_err(|_| Errno::OutOfRange)?;
        } else {
            store.unset(R::name(key));
        }
    }
    store.commit()
}

/// Remove every key of the registry from the user's own document, so the
/// layers beneath it apply again: what *Restore defaults* means, since the
/// record that then applies may be the machine's policy rather than the
/// application's own default.
///
/// # Errors
///
/// The service's refusal of the commit.
pub fn clear<R: Registry>(store: &mut Settings<'_>) -> Result<(), Errno> {
    for &key in R::KEYS {
        store.unset(R::name(key));
    }
    store.commit()
}

/// The record `store` implies, and everything about reading it worth saying:
/// a store the service could not serve, shipped defaults that could not be
/// used, and every stored value the registry refused.
#[must_use]
pub fn loaded<R: Registry>(store: &Settings<'_>) -> (R, Vec<Refusal>) {
    let mut refusals = Vec::new();
    if let Some(err) = store.store_refusal() {
        refusals.push(Refusal::StoreUnreadable(err));
    }
    if let Some(err) = store.defaults_refusal() {
        refusals.push(Refusal::DefaultsUnreadable(err));
    }
    let (record, refused) = R::load(store);
    refusals.extend(
        refused
            .into_iter()
            .map(|key| Refusal::Unusable(R::name(key))),
    );
    (record, refusals)
}

/// What a store write is asked to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublishJob<R> {
    /// Write this record.
    Save(R),
    /// Remove the user's opinions, so the layers beneath them apply again.
    Restore,
}

/// What the store said after a write: the record it now implies, and the
/// stored values the registry refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Published<R: Registry> {
    /// The record the store's layers now imply.
    pub record: R,
    /// Each key whose stored value could not be used.
    pub refused: Vec<R::Key>,
}

/// Carry out `job` against `store`, answering what the store then implies —
/// what was asked for wherever a layer does not say otherwise.
///
/// # Errors
///
/// A store the service could not serve, which is not written over, and the
/// refusals of [`save`] and [`clear`].
pub fn publish<R: Registry>(
    store: &mut Settings<'_>,
    job: &PublishJob<R>,
) -> Result<Published<R>, Errno> {
    if let Some(err) = store.store_refusal() {
        return Err(err);
    }
    match job {
        PublishJob::Save(record) => save(record, store)?,
        PublishJob::Restore => clear::<R>(store)?,
    }
    let (record, refused) = R::load(store);
    Ok(Published { record, refused })
}

/// Something about a registry's store worth saying, never fatal: the
/// settings in force carry on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// The store could not be served; the shipped defaults stand.
    StoreUnreadable(Errno),
    /// The bundle's shipped defaults exist and could not be read.
    DefaultsUnreadable(Errno),
    /// A stored value the key named does not accept; its default stands.
    Unusable(&'static str),
    /// The store refused a save.
    NotSaved(Errno),
    /// The store refused a restore.
    NotRestored(Errno),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StoreUnreadable(err) => {
                write!(
                    f,
                    "settings unavailable ({err}); running on this build's defaults"
                )
            }
            Self::DefaultsUnreadable(err) => {
                write!(
                    f,
                    "this bundle's shipped defaults could not be read ({err})"
                )
            }
            Self::Unusable(name) => {
                write!(
                    f,
                    "{name}: not a value this setting accepts; using its default"
                )
            }
            Self::NotSaved(err) => write!(
                f,
                "the settings were not saved ({err}); keeping the settings in force"
            ),
            Self::NotRestored(err) => write!(
                f,
                "the defaults were not restored ({err}); keeping the settings in force"
            ),
        }
    }
}

/// The record in force, the one on screen, and the one the store last said.
///
/// `adopted` and `live` differ only in settings edited since and in those the
/// outstanding write carries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Publication<R: Live> {
    adopted: R,
    live: R,
    rendered: R,
    /// The settings changed that no asked-for write carries: what an answer
    /// leaves as they are.
    edited: Keys<R>,
    outstanding: Option<Asked>,
    owed: Owed,
}

/// Which write is outstanding, so a refusal names what failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Asked {
    Save,
    Restore,
}

/// What was asked for while a write was outstanding. A restore goes first,
/// as a save asked for after it describes the record the restore leaves.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Owed {
    restore: bool,
    save: bool,
}

impl<R: Live> Publication<R> {
    /// Start from the record the store implied at start.
    #[must_use]
    pub fn new(record: R) -> Self {
        Self {
            adopted: record.clone(),
            live: record.clone(),
            rendered: record,
            edited: Keys::EMPTY,
            outstanding: None,
            owed: Owed::default(),
        }
    }

    /// The record in force.
    #[must_use]
    pub const fn live(&self) -> &R {
        &self.live
    }

    /// The record the store last said.
    #[must_use]
    pub const fn adopted(&self) -> &R {
        &self.adopted
    }

    /// Show one edit without writing anything: the settings `was` and `now` —
    /// an editor's record either side of one event — disagree on are taken
    /// from `now`, so an editor whose copy has fallen behind cannot put its
    /// stale values back.
    pub fn edit(&mut self, was: &R, now: &R) {
        let touched = was.differing(now);
        self.live.set_from(now, touched);
        self.edited = self.edited.union(touched);
    }

    /// What the surface has yet to draw, as `between` reads the record it
    /// last drew against the one in force; taking it records the live one as
    /// drawn, so a burst of edits costs one repaint and none is lost on the
    /// way.
    pub fn take_pending<T>(&mut self, between: impl FnOnce(&R, &R) -> T) -> T {
        let pending = between(&self.rendered, &self.live);
        self.rendered.clone_from(&self.live);
        pending
    }

    /// Ask for the edits to be written, the interaction that made them having
    /// settled, answering the write to submit now: none when nothing is
    /// unwritten, or when one is outstanding, behind which this one is owed.
    #[must_use]
    pub fn settle(&mut self) -> Option<PublishJob<R>> {
        if !self.edited.is_empty() {
            self.owed.save = true;
        }
        self.next()
    }

    /// Ask for the user's opinions to be removed, with every edit not yet
    /// written, answering as [`settle`](Self::settle) does. Nothing on screen
    /// changes until the store answers: what applies then is its to say.
    #[must_use]
    pub fn restore(&mut self) -> Option<PublishJob<R>> {
        self.edited = Keys::EMPTY;
        self.owed = Owed {
            restore: true,
            save: false,
        };
        self.next()
    }

    /// Adopt what the store said about the outstanding write, or why it said
    /// nothing, and answer the owed write to submit next.
    ///
    /// The answer applies to every setting not edited since the write was
    /// asked for; a refusal puts those back to what the store last held. A
    /// setting edited since keeps its value either way: the answer describes a
    /// moment the user has moved on from, and that setting's own settle
    /// writes it. `refusals` receives what is worth saying.
    #[must_use]
    pub fn adopt(
        &mut self,
        answer: Result<Published<R>, Errno>,
        refusals: &mut Vec<Refusal>,
    ) -> Option<PublishJob<R>> {
        let asked = self.outstanding.take();
        let mut record = match answer {
            Ok(published) => {
                refusals.extend(
                    published
                        .refused
                        .iter()
                        .map(|&key| Refusal::Unusable(R::name(key))),
                );
                self.adopted.clone_from(&published.record);
                published.record
            }
            Err(err) => {
                refusals.push(match asked {
                    Some(Asked::Restore) => Refusal::NotRestored(err),
                    Some(Asked::Save) | None => Refusal::NotSaved(err),
                });
                self.adopted.clone()
            }
        };
        record.set_from(&self.live, self.edited);
        self.live = record;
        self.next()
    }

    /// Hand out the owed write, if one is owed and none is outstanding.
    fn next(&mut self) -> Option<PublishJob<R>> {
        if self.outstanding.is_some() {
            return None;
        }
        if self.owed.restore {
            self.owed.restore = false;
            self.outstanding = Some(Asked::Restore);
            return Some(PublishJob::Restore);
        }
        if !core::mem::take(&mut self.owed.save) || self.edited.is_empty() {
            return None;
        }
        self.edited = Keys::EMPTY;
        // Edits that came back to rest on what the store holds leave nothing
        // to write.
        if self.live == self.adopted {
            return None;
        }
        self.outstanding = Some(Asked::Save);
        Some(PublishJob::Save(self.live.clone()))
    }
}

#[cfg(test)]
#[path = "publish_tests.rs"]
mod tests;
