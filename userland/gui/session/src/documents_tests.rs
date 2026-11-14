use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::PublisherId;

use super::{open_documents_folder, owner_mark, DocumentsFs};
use tairix_abi::window_ipc::DocumentsRefusal;
use tairix_abi::{AppIdentity, Errno};

/// What a name beneath the user's documents holds.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Entry {
    Folder(Option<Vec<u8>>),
    File,
    Link,
}

/// Documents in memory, refusing as asked.
#[derive(Default)]
struct Fake {
    entries: BTreeMap<String, Entry>,
    unreachable: bool,
    refuse_make: Option<Errno>,
    /// What another takes the name with between the open and the make.
    raced: Option<Entry>,
}

impl DocumentsFs for Fake {
    type Documents = ();
    type Folder = String;

    fn documents(&mut self) -> Result<(), Errno> {
        if self.unreachable {
            Err(Errno::NoSpace)
        } else {
            Ok(())
        }
    }

    fn open(&mut self, (): &(), name: &str) -> Result<String, Errno> {
        match self.entries.get(name) {
            None => Err(Errno::NotFound),
            Some(Entry::Folder(_)) => Ok(name.to_string()),
            Some(Entry::File) => Err(Errno::NotADirectory),
            Some(Entry::Link) => Err(Errno::LinkLoop),
        }
    }

    fn make(&mut self, (): &(), name: &str, mark: &[u8]) -> Result<(), Errno> {
        if let Some(entry) = self.raced.take() {
            self.entries.insert(name.to_string(), entry);
            return Err(Errno::AlreadyExists);
        }
        if let Some(err) = self.refuse_make {
            return Err(err);
        }
        self.entries
            .insert(name.to_string(), Entry::Folder(Some(mark.to_vec())));
        Ok(())
    }

    fn mark(&mut self, folder: &String) -> Result<Option<Vec<u8>>, Errno> {
        match self.entries.get(folder) {
            Some(Entry::Folder(mark)) => Ok(mark.clone()),
            _ => Err(Errno::NotFound),
        }
    }
}

fn app(bundle: &str, publisher: u8) -> AppIdentity {
    AppIdentity::new(bundle, PublisherId::from_raw([publisher; 32])).expect("an identity")
}

const NAME: &str = "Open Micro";

#[test]
fn a_missing_folder_is_made_marked_and_granted() {
    let micro = app("os.tairix.openmicro", 7);
    let mut fs = Fake::default();
    assert_eq!(
        open_documents_folder(&mut fs, NAME, &micro),
        Ok(NAME.to_string())
    );
    assert_eq!(
        fs.entries.get(NAME),
        Some(&Entry::Folder(Some(owner_mark(&micro))))
    );
    assert_eq!(
        open_documents_folder(&mut fs, NAME, &micro),
        Ok(NAME.to_string()),
        "granted again as its own"
    );
}

#[test]
fn a_folder_another_marked_or_the_user_made_is_not_theirs() {
    let micro = app("os.tairix.openmicro", 7);
    for (holder, why) in [
        (
            Some(owner_mark(&app("com.example.other", 7))),
            "another bundle",
        ),
        (
            Some(owner_mark(&app("os.tairix.openmicro", 8))),
            "another publisher",
        ),
        (None, "the user's own"),
    ] {
        let mut fs = Fake::default();
        fs.entries.insert(NAME.to_string(), Entry::Folder(holder));
        assert_eq!(
            open_documents_folder(&mut fs, NAME, &micro),
            Err(DocumentsRefusal::NotTheirs),
            "{why}"
        );
    }
}

#[test]
fn a_file_or_a_link_at_the_name_is_no_folder() {
    let micro = app("os.tairix.openmicro", 7);
    for entry in [Entry::File, Entry::Link] {
        let mut fs = Fake::default();
        fs.entries.insert(NAME.to_string(), entry);
        assert_eq!(
            open_documents_folder(&mut fs, NAME, &micro),
            Err(DocumentsRefusal::NotAFolder)
        );
    }
}

#[test]
fn documents_out_of_reach_or_a_folder_that_cannot_be_made_is_unavailable() {
    let micro = app("os.tairix.openmicro", 7);
    let mut unreachable = Fake {
        unreachable: true,
        ..Fake::default()
    };
    assert_eq!(
        open_documents_folder(&mut unreachable, NAME, &micro),
        Err(DocumentsRefusal::Unavailable)
    );
    let mut full = Fake {
        refuse_make: Some(Errno::NoSpace),
        ..Fake::default()
    };
    assert_eq!(
        open_documents_folder(&mut full, NAME, &micro),
        Err(DocumentsRefusal::Unavailable)
    );
    assert!(full.entries.is_empty(), "nothing left behind");
}

/// A name taken between the open and the make is judged by what took it.
#[test]
fn a_folder_made_by_another_first_is_judged_by_its_mark() {
    let micro = app("os.tairix.openmicro", 7);
    let mut ours = Fake {
        raced: Some(Entry::Folder(Some(owner_mark(&micro)))),
        ..Fake::default()
    };
    assert_eq!(
        open_documents_folder(&mut ours, NAME, &micro),
        Ok(NAME.to_string())
    );
    let mut theirs = Fake {
        raced: Some(Entry::Folder(None)),
        ..Fake::default()
    };
    assert_eq!(
        open_documents_folder(&mut theirs, NAME, &micro),
        Err(DocumentsRefusal::NotTheirs)
    );
}

#[test]
fn the_mark_is_the_publisher_then_the_bundle_id() {
    let mark = owner_mark(&app("os.tairix.openmicro", 7));
    assert_eq!(&mark[..32], &[7; 32]);
    assert_eq!(&mark[32..], b"os.tairix.openmicro");
}
