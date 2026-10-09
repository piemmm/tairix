//! A registry kept in the private scope, and the publication that keeps a
//! surface live while it is written: the store half over the fake service,
//! the publication half over records alone.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_abi::appdata_ipc::APPDATA_DOCUMENT_MAX;
use tairix_abi::Errno;
use tairix_appconf::{as_u32, overwrite, Keys, Live, Registry};

use super::{clear, loaded, publish, save, Publication, PublishJob, Published, Refusal};
use crate::fake::FakeService;
use crate::Settings;

const OWN_WORD: &str = "notes";
const BUNDLE: &str = "/System/Applications/notes.app";

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Key {
    Scheme,
    Size,
    Opacity,
    Badge,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
enum Scheme {
    #[default]
    System,
    Contrast,
    Amber,
}

impl Scheme {
    const ALL: [Self; 3] = [Self::System, Self::Contrast, Self::Amber];

    const fn name(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Contrast => "contrast",
            Self::Amber => "amber",
        }
    }
}

/// A look like a terminal's: a scheme, a text size a slider drives, an
/// opacity, and a badge a store says is absent by not holding it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Look {
    scheme: Scheme,
    size: u32,
    opacity: u32,
    badge: Option<u32>,
}

impl Default for Look {
    fn default() -> Self {
        Self {
            scheme: Scheme::System,
            size: 14,
            opacity: 1000,
            badge: None,
        }
    }
}

impl Registry for Look {
    type Key = Key;
    const KEYS: &'static [Key] = &[Key::Scheme, Key::Size, Key::Opacity, Key::Badge];

    fn name(key: Key) -> &'static str {
        match key {
            Key::Scheme => "scheme",
            Key::Size => "font.size",
            Key::Opacity => "opacity",
            Key::Badge => "badge",
        }
    }

    fn read(&mut self, key: Key, text: &str) -> bool {
        let number = as_u32(text).ok();
        match key {
            Key::Scheme => match Scheme::ALL.into_iter().find(|scheme| scheme.name() == text) {
                Some(scheme) => self.scheme = scheme,
                None => return false,
            },
            Key::Size => match number.filter(|size| (8..=48).contains(size)) {
                Some(size) => self.size = size,
                None => return false,
            },
            Key::Opacity => match number.filter(|&opacity| opacity <= 1000) {
                Some(opacity) => self.opacity = opacity,
                None => return false,
            },
            Key::Badge => match number {
                Some(badge) => self.badge = Some(badge),
                None => return false,
            },
        }
        true
    }

    fn spell(&self, key: Key, out: &mut String) -> bool {
        match key {
            Key::Scheme => {
                out.push_str(self.scheme.name());
                true
            }
            Key::Size => write!(out, "{}", self.size).is_ok(),
            Key::Opacity => write!(out, "{}", self.opacity).is_ok(),
            Key::Badge => self
                .badge
                .is_some_and(|badge| write!(out, "{badge}").is_ok()),
        }
    }
}

impl Live for Look {
    fn take(&mut self, other: &Self, key: Key) -> bool {
        match key {
            Key::Scheme => overwrite(&mut self.scheme, other.scheme),
            Key::Size => overwrite(&mut self.size, other.size),
            Key::Opacity => overwrite(&mut self.opacity, other.opacity),
            Key::Badge => overwrite(&mut self.badge, other.badge),
        }
    }
}

fn service() -> FakeService {
    FakeService::for_word(OWN_WORD).with_bundle(BUNDLE)
}

// --- The store ---------------------------------------------------------------

#[test]
fn a_save_writes_only_what_the_layers_do_not_already_say() {
    let mut host = FakeService::for_word(OWN_WORD).with_defaults(BUNDLE, "opacity = 900\n");
    let mut store = Settings::open(&mut host, OWN_WORD);
    let (shipped, refusals) = loaded::<Look>(&store);
    assert_eq!((shipped.opacity, refusals), (900, alloc::vec![]));
    let mine = Look {
        size: 20,
        ..shipped
    };
    save(&mine, &mut store).expect("written");
    drop(store);
    let document = host.committed();
    assert_eq!(document.get("font.size"), Some("20"));
    assert_eq!(
        document.get("opacity"),
        None,
        "the bundle's value is not copied up"
    );
    assert_eq!(document.get("scheme"), None);
}

#[test]
fn a_save_mends_a_stored_value_the_registry_refused_and_spares_another_apps_keys() {
    let mut host = service().with_store("font.size = huge\nlayout.other = kept\n");
    let mut store = Settings::open(&mut host, OWN_WORD);
    let (record, refusals) = loaded::<Look>(&store);
    assert_eq!(refusals, [Refusal::Unusable("font.size")]);
    save(&record, &mut store).expect("written");
    drop(store);
    assert_eq!(host.committed().get("font.size"), Some("14"));
    assert_eq!(host.committed().get("layout.other"), Some("kept"));
}

#[test]
fn a_setting_said_by_its_absence_is_removed_and_a_restore_removes_every_key() {
    let mut host = service().with_store("badge = 3\nscheme = amber\nlayout.other = kept\n");
    let mut store = Settings::open(&mut host, OWN_WORD);
    let (mut record, _) = loaded::<Look>(&store);
    assert_eq!(record.badge, Some(3));
    record.badge = None;
    save(&record, &mut store).expect("written");
    assert_eq!(store.get("badge"), None);
    assert_eq!(store.get("scheme"), Some("amber"));
    clear::<Look>(&mut store).expect("cleared");
    drop(store);
    assert_eq!(host.committed().get("scheme"), None);
    assert_eq!(
        host.committed().get("layout.other"),
        Some("kept"),
        "not the registry's"
    );
}

#[test]
fn a_publish_answers_what_the_store_then_says() {
    let mut host = FakeService::for_word(OWN_WORD).with_defaults(BUNDLE, "scheme = contrast\n");
    let mut store = Settings::open(&mut host, OWN_WORD);
    let (shipped, _) = loaded::<Look>(&store);
    let mine = Look {
        size: 30,
        ..shipped
    };
    let saved = publish(&mut store, &PublishJob::Save(mine)).expect("written");
    assert_eq!(saved.record, mine);
    assert_eq!(
        store.get("scheme"),
        Some("contrast"),
        "the shipped scheme stands beneath"
    );
    let restored = publish::<Look>(&mut store, &PublishJob::Restore).expect("restored");
    assert_eq!(restored.record, shipped);
    let chosen = Look {
        scheme: Scheme::Amber,
        ..shipped
    };
    let saved = publish(&mut store, &PublishJob::Save(chosen)).expect("written");
    assert_eq!(
        saved.record.scheme,
        Scheme::Amber,
        "a choice over the shipped one is the user's"
    );
    drop(store);
    assert_eq!(host.committed().get("scheme"), Some("amber"));
}

#[test]
fn a_store_that_could_not_be_read_is_not_written_over_and_is_said() {
    let mut host = service().with_store("font.size = 20\n");
    host.refusal().set(Some(Errno::NotFound));
    let mut store = Settings::open(&mut host, OWN_WORD);
    let (record, refusals) = loaded::<Look>(&store);
    assert_eq!(record, Look::default());
    assert_eq!(refusals, [Refusal::StoreUnreadable(Errno::NotFound)]);
    assert_eq!(
        publish(&mut store, &PublishJob::Save(record)),
        Err(Errno::NotFound)
    );
}

#[test]
fn broken_shipped_defaults_are_said() {
    let oversize: String = core::iter::repeat_n('x', APPDATA_DOCUMENT_MAX + 1).collect();
    let mut host = FakeService::for_word(OWN_WORD).with_defaults(BUNDLE, &oversize);
    let store = Settings::open(&mut host, OWN_WORD);
    let (_, refusals) = loaded::<Look>(&store);
    assert_eq!(
        refusals,
        [Refusal::DefaultsUnreadable(Errno::LengthOutOfRange)]
    );
}

#[test]
fn every_refusal_says_what_carries_on() {
    let said = |refusal: Refusal| alloc::format!("{refusal}");
    assert!(said(Refusal::Unusable("font.size")).starts_with("font.size: not a value"));
    assert!(said(Refusal::NotSaved(Errno::NoSpace)).starts_with("the settings were not saved"));
    assert!(
        said(Refusal::NotRestored(Errno::NoSpace)).starts_with("the defaults were not restored")
    );
    for refusal in [
        Refusal::NotSaved(Errno::NoSpace),
        Refusal::StoreUnreadable(Errno::NoSpace),
    ] {
        assert!(said(refusal).contains(&alloc::format!("{}", Errno::NoSpace)));
    }
}

// --- The publication ---------------------------------------------------------

fn sized(steps: u32) -> Look {
    Look {
        size: Look::default().size + steps,
        ..Look::default()
    }
}

fn larger() -> Look {
    sized(4)
}

fn largest() -> Look {
    sized(8)
}

/// One edit by an editor in step with the live record: a sample of a drag.
fn edit(publication: &mut Publication<Look>, change: impl FnOnce(&mut Look)) {
    let was = *publication.live();
    let mut now = was;
    change(&mut now);
    publication.edit(&was, &now);
}

fn resize_to(look: Look) -> impl FnOnce(&mut Look) {
    move |now| now.size = look.size
}

fn set_opacity(opacity: u32) -> impl FnOnce(&mut Look) {
    move |now| now.opacity = opacity
}

fn confirm(publication: &mut Publication<Look>, record: Look) -> Option<PublishJob<Look>> {
    let answer = Published {
        record,
        refused: Vec::new(),
    };
    publication.adopt(Ok(answer), &mut Vec::new())
}

fn refuse(publication: &mut Publication<Look>, err: Errno) -> Option<PublishJob<Look>> {
    publication.adopt(Err(err), &mut Vec::new())
}

fn pending(publication: &mut Publication<Look>) -> Keys<Look> {
    publication.take_pending(Look::differing)
}

/// Persisting per sample would be one round trip and one commit per pointer
/// motion.
#[test]
fn a_drag_shows_every_sample_and_writes_nothing() {
    let mut publication = Publication::new(Look::default());
    for steps in 1..=8 {
        edit(&mut publication, resize_to(sized(steps)));
    }
    assert_eq!(*publication.live(), largest());
    assert_eq!(*publication.adopted(), Look::default());
}

#[test]
fn a_settle_asks_for_exactly_one_write() {
    let mut publication = Publication::new(Look::default());
    edit(&mut publication, resize_to(larger()));
    edit(&mut publication, resize_to(largest()));
    assert_eq!(publication.settle(), Some(PublishJob::Save(largest())));
    assert_eq!(publication.settle(), None, "nothing is left unwritten");
    assert_eq!(*publication.adopted(), Look::default());
    assert_eq!(confirm(&mut publication, largest()), None, "nothing owed");
}

#[test]
fn settling_with_nothing_edited_or_edits_come_back_to_rest_writes_nothing() {
    let mut publication = Publication::new(Look::default());
    assert_eq!(publication.settle(), None);
    let unchanged = *publication.live();
    publication.edit(&unchanged, &unchanged);
    assert_eq!(publication.settle(), None);
    edit(&mut publication, resize_to(larger()));
    edit(&mut publication, resize_to(Look::default()));
    assert_eq!(publication.settle(), None);
}

/// A lower layer the user's document does not override still wins.
#[test]
fn the_stores_answer_is_what_gets_adopted() {
    let mut publication = Publication::new(Look::default());
    edit(&mut publication, resize_to(largest()));
    let _ = publication.settle();
    assert_eq!(confirm(&mut publication, larger()), None);
    assert_eq!(*publication.adopted(), larger());
    assert_eq!(*publication.live(), larger());
}

#[test]
fn a_refused_write_reverts_the_edit_and_says_why() {
    let mut publication = Publication::new(Look::default());
    edit(&mut publication, resize_to(largest()));
    let _ = publication.settle();
    let mut refusals = Vec::new();
    assert_eq!(publication.adopt(Err(Errno::NoSpace), &mut refusals), None);
    assert_eq!(*publication.live(), Look::default());
    assert_eq!(refusals, [Refusal::NotSaved(Errno::NoSpace)]);
}

#[test]
fn a_confirmed_write_owes_the_screen_nothing() {
    let mut publication = Publication::new(Look::default());
    edit(&mut publication, resize_to(largest()));
    let _ = pending(&mut publication);
    let _ = publication.settle();
    let _ = confirm(&mut publication, largest());
    assert!(pending(&mut publication).is_empty());
}

#[test]
fn a_restore_asks_the_store_and_adopts_its_answer() {
    let mut publication = Publication::new(largest());
    assert_eq!(publication.restore(), Some(PublishJob::Restore));
    assert_eq!(
        *publication.live(),
        largest(),
        "nothing changes until the store speaks"
    );
    let policy = Look {
        scheme: Scheme::Contrast,
        ..Look::default()
    };
    assert_eq!(confirm(&mut publication, policy), None);
    assert_eq!(*publication.live(), policy);
}

#[test]
fn a_refused_restore_says_the_defaults_were_not_restored() {
    let mut publication = Publication::new(largest());
    let _ = publication.restore();
    let mut refusals = Vec::new();
    let _ = publication.adopt(Err(Errno::PermissionDenied), &mut refusals);
    assert_eq!(*publication.live(), largest());
    assert_eq!(refusals, [Refusal::NotRestored(Errno::PermissionDenied)]);
}

#[test]
fn the_values_an_answer_refused_are_said() {
    let mut publication = Publication::new(Look::default());
    edit(&mut publication, resize_to(larger()));
    let _ = publication.settle();
    let mut refusals = Vec::new();
    let answer = Published {
        record: larger(),
        refused: alloc::vec![Key::Scheme],
    };
    let _ = publication.adopt(Ok(answer), &mut refusals);
    assert_eq!(refusals, [Refusal::Unusable("scheme")]);
}

#[test]
fn an_answer_landing_mid_drag_leaves_the_dragged_setting_alone() {
    let mut publication = Publication::new(Look::default());
    edit(&mut publication, resize_to(larger()));
    assert_eq!(publication.settle(), Some(PublishJob::Save(larger())));
    edit(&mut publication, resize_to(sized(5)));
    assert_eq!(confirm(&mut publication, larger()), None);
    assert_eq!(*publication.live(), sized(5), "not snapped back");
    edit(&mut publication, resize_to(largest()));
    assert_eq!(publication.settle(), Some(PublishJob::Save(largest())));
}

#[test]
fn a_setting_dragged_back_to_its_written_value_is_still_the_users() {
    let mut publication = Publication::new(Look::default());
    edit(&mut publication, resize_to(larger()));
    let _ = publication.settle();
    edit(&mut publication, resize_to(largest()));
    edit(&mut publication, resize_to(larger()));
    let _ = confirm(&mut publication, Look::default());
    assert_eq!(*publication.live(), larger());
    assert_eq!(*publication.adopted(), Look::default());
}

#[test]
fn a_policy_answer_mid_drag_wins_everywhere_the_user_is_not_editing() {
    let mut publication = Publication::new(Look::default());
    edit(&mut publication, resize_to(larger()));
    let _ = publication.settle();
    edit(&mut publication, set_opacity(500));
    let policy = Look {
        scheme: Scheme::Contrast,
        ..larger()
    };
    let _ = confirm(&mut publication, policy);
    let live = *publication.live();
    assert_eq!(
        (live.scheme, live.size, live.opacity),
        (Scheme::Contrast, larger().size, 500)
    );
}

#[test]
fn a_restore_answered_mid_drag_is_not_undone_by_the_drags_settle() {
    let opinions = Look {
        scheme: Scheme::Amber,
        ..largest()
    };
    let mut publication = Publication::new(opinions);
    assert_eq!(publication.restore(), Some(PublishJob::Restore));
    edit(&mut publication, set_opacity(500));
    let restored = Look {
        scheme: Scheme::Contrast,
        ..Look::default()
    };
    assert_eq!(confirm(&mut publication, restored), None);
    let expected = Look {
        opacity: 500,
        ..restored
    };
    assert_eq!(*publication.live(), expected);
    assert_eq!(publication.settle(), Some(PublishJob::Save(expected)));
}

#[test]
fn a_refusal_landing_mid_drag_leaves_the_dragged_setting_alone() {
    let mut publication = Publication::new(Look::default());
    edit(&mut publication, resize_to(larger()));
    let _ = publication.settle();
    edit(&mut publication, set_opacity(500));
    let mut refusals = Vec::new();
    assert_eq!(publication.adopt(Err(Errno::NoSpace), &mut refusals), None);
    assert_eq!(refusals.len(), 1);
    let expected = Look {
        opacity: 500,
        ..Look::default()
    };
    assert_eq!(*publication.live(), expected);
    assert_eq!(publication.settle(), Some(PublishJob::Save(expected)));

    let mut same = Publication::new(Look::default());
    edit(&mut same, resize_to(larger()));
    let _ = same.settle();
    edit(&mut same, resize_to(largest()));
    let _ = refuse(&mut same, Errno::NoSpace);
    assert_eq!(
        *same.live(),
        largest(),
        "the setting under the pointer stays"
    );
    assert_eq!(same.settle(), Some(PublishJob::Save(largest())));
}

#[test]
fn a_save_asked_for_while_a_write_is_outstanding_waits_for_its_answer() {
    let mut publication = Publication::new(Look::default());
    edit(&mut publication, resize_to(larger()));
    assert_eq!(publication.settle(), Some(PublishJob::Save(larger())));
    edit(&mut publication, resize_to(largest()));
    assert_eq!(publication.settle(), None, "one write at a time");
    assert_eq!(
        confirm(&mut publication, larger()),
        Some(PublishJob::Save(largest()))
    );
    assert_eq!(confirm(&mut publication, largest()), None);
    assert_eq!(*publication.adopted(), largest());
}

#[test]
fn a_restore_behind_a_write_is_not_displaced_by_a_later_save() {
    let mut publication = Publication::new(Look::default());
    edit(&mut publication, resize_to(larger()));
    let _ = publication.settle();
    assert_eq!(publication.restore(), None, "owed behind the write");
    edit(&mut publication, set_opacity(500));
    assert_eq!(publication.settle(), None, "owed behind the restore");
    assert_eq!(
        confirm(&mut publication, larger()),
        Some(PublishJob::Restore)
    );
    let restored = Look {
        scheme: Scheme::Contrast,
        ..Look::default()
    };
    let expected = Look {
        opacity: 500,
        ..restored
    };
    assert_eq!(
        confirm(&mut publication, restored),
        Some(PublishJob::Save(expected))
    );
}

#[test]
fn an_edit_made_before_a_restore_is_dropped_with_the_users_opinions() {
    let mut publication = Publication::new(Look::default());
    edit(&mut publication, resize_to(larger()));
    let _ = publication.settle();
    edit(&mut publication, resize_to(largest()));
    assert_eq!(publication.settle(), None);
    assert_eq!(publication.restore(), None);
    assert_eq!(
        confirm(&mut publication, larger()),
        Some(PublishJob::Restore)
    );
    assert_eq!(confirm(&mut publication, Look::default()), None);
    assert_eq!(*publication.live(), Look::default());
}

#[test]
fn an_editor_holding_a_stale_copy_changes_only_what_it_edited() {
    let mut publication = Publication::new(Look::default());
    let stale = *publication.live();
    edit(&mut publication, resize_to(larger()));
    let edited = Look {
        opacity: 500,
        ..stale
    };
    publication.edit(&stale, &edited);
    assert_eq!(
        (publication.live().size, publication.live().opacity),
        (larger().size, 500)
    );
}

/// What is owed is measured against the screen, not against each edit.
#[test]
fn a_burst_of_edits_owes_one_paint_and_an_undrawn_edit_stays_owed() {
    let mut publication = Publication::new(Look::default());
    for steps in 1..=8 {
        edit(&mut publication, resize_to(sized(steps)));
    }
    assert_eq!(pending(&mut publication), Keys::of(Key::Size));
    assert!(pending(&mut publication).is_empty(), "nothing once drawn");
    edit(&mut publication, resize_to(larger()));
    edit(&mut publication, resize_to(largest()));
    assert!(
        pending(&mut publication).is_empty(),
        "back to what the screen shows"
    );
    edit(&mut publication, resize_to(larger()));
    assert_eq!(pending(&mut publication), Keys::of(Key::Size));
}

#[test]
fn an_adopted_answer_or_a_revert_owes_the_difference_from_the_screen() {
    let mut publication = Publication::new(Look::default());
    edit(&mut publication, resize_to(larger()));
    let _ = pending(&mut publication);
    let _ = publication.settle();
    let _ = confirm(&mut publication, largest());
    assert_eq!(pending(&mut publication), Keys::of(Key::Size));

    let mut refused = Publication::new(Look::default());
    edit(&mut refused, resize_to(larger()));
    let _ = pending(&mut refused);
    let _ = refused.settle();
    let _ = refuse(&mut refused, Errno::PermissionDenied);
    assert_eq!(
        pending(&mut refused),
        Keys::of(Key::Size),
        "owes the revert"
    );
}
