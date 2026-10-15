use super::{parent_of, replace, staged_path, ParentDir, ReplaceFs, Security, TAG_LEN};

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use tairix_abi::Errno;

const TAG: [u8; TAG_LEN] = [0x0f; TAG_LEN];
const DOC: &[u8] = b"/Settings/doc";
const OLD: &[u8] = b"old document";
const NEW: &[u8] = b"the new document, longer than the old";

const OWNER: Security = Security {
    uid: 18,
    gid: 18,
    mode: 0o640,
};
const CREATOR: Security = Security {
    uid: 0,
    gid: 0,
    mode: 0o644,
};

#[derive(Clone)]
struct Node {
    data: Vec<u8>,
    security: Security,
}

#[derive(Clone, Default)]
struct State {
    names: BTreeMap<Vec<u8>, usize>,
    dirs: BTreeSet<Vec<u8>>,
    nodes: Vec<Node>,
}

impl State {
    fn read(&self, path: &[u8]) -> Option<&[u8]> {
        self.names
            .get(path)
            .map(|&node| self.nodes[node].data.as_slice())
    }
}

/// A name or security change, which a crash may keep without the data the
/// writer meant it to cover.
#[derive(Clone)]
enum Change {
    Create(Vec<u8>, usize),
    Rename(Vec<u8>, Vec<u8>),
    Security(usize, Security),
    Unlink(Vec<u8>),
    Mkdir(Vec<u8>),
}

impl Change {
    fn apply(&self, state: &mut State) {
        match self {
            Self::Create(path, node) => {
                if state.nodes.len() <= *node {
                    state.nodes.resize(
                        node + 1,
                        Node {
                            data: Vec::new(),
                            security: CREATOR,
                        },
                    );
                }
                state.names.insert(path.clone(), *node);
            }
            Self::Rename(from, to) => {
                if let Some(node) = state.names.remove(from) {
                    state.names.insert(to.clone(), node);
                }
            }
            Self::Security(node, security) => state.nodes[*node].security = *security,
            Self::Unlink(path) => {
                state.names.remove(path);
            }
            Self::Mkdir(path) => {
                state.dirs.insert(path.clone());
            }
        }
    }
}

/// A medium that keeps only what a sync put on it: after a crash, every name
/// and security change since the last sync may survive while no write since
/// it does, which is the worst a write-back cache can do.
struct Medium {
    now: State,
    synced: State,
    since: Vec<Change>,
    calls_left: usize,
    refuse_write: bool,
}

impl Medium {
    fn new(document: Option<&[u8]>) -> Self {
        let mut state = State::default();
        state.dirs.insert(b"/Settings".to_vec());
        if let Some(bytes) = document {
            state.nodes.push(Node {
                data: bytes.to_vec(),
                security: OWNER,
            });
            state.names.insert(DOC.to_vec(), 0);
        }
        Self {
            now: state.clone(),
            synced: state,
            since: Vec::new(),
            calls_left: usize::MAX,
            refuse_write: false,
        }
    }

    /// What the medium holds after a crash now.
    fn after_crash(&self) -> State {
        let mut state = self.synced.clone();
        for change in &self.since {
            change.apply(&mut state);
        }
        state
    }

    fn call(&mut self) -> Result<(), Errno> {
        if self.calls_left == 0 {
            return Err(Errno::DeviceOffline);
        }
        self.calls_left -= 1;
        Ok(())
    }

    fn change(&mut self, change: Change) {
        change.apply(&mut self.now);
        self.since.push(change);
    }

    fn node(&self, path: &[u8]) -> Result<usize, Errno> {
        self.now.names.get(path).copied().ok_or(Errno::NotFound)
    }
}

impl ReplaceFs for Medium {
    type File = usize;

    fn security_of_path(&mut self, path: &[u8]) -> Result<Option<Security>, Errno> {
        self.call()?;
        Ok(self
            .now
            .names
            .get(path)
            .map(|&node| self.now.nodes[node].security))
    }

    fn create_new(&mut self, path: &[u8]) -> Result<usize, Errno> {
        self.call()?;
        let parent = parent_of(path).ok_or(Errno::NotFound)?;
        if !self.now.dirs.contains(parent) {
            return Err(Errno::NotFound);
        }
        if self.now.names.contains_key(path) {
            return Err(Errno::AlreadyExists);
        }
        let node = self.now.nodes.len();
        self.change(Change::Create(path.to_vec(), node));
        Ok(node)
    }

    fn security_of(&mut self, file: &usize) -> Result<Security, Errno> {
        self.call()?;
        Ok(self.now.nodes[*file].security)
    }

    fn set_owner(&mut self, path: &[u8], uid: u32, gid: u32) -> Result<(), Errno> {
        self.call()?;
        let node = self.node(path)?;
        let mut security = self.now.nodes[node].security;
        security.uid = uid;
        security.gid = gid;
        self.change(Change::Security(node, security));
        Ok(())
    }

    fn set_mode(&mut self, path: &[u8], mode: u32) -> Result<(), Errno> {
        self.call()?;
        let node = self.node(path)?;
        let mut security = self.now.nodes[node].security;
        security.mode = mode;
        self.change(Change::Security(node, security));
        Ok(())
    }

    fn write_all(&mut self, file: &usize, bytes: &[u8]) -> Result<(), Errno> {
        self.call()?;
        if self.refuse_write {
            return Err(Errno::NoSpace);
        }
        self.now.nodes[*file].data = bytes.to_vec();
        Ok(())
    }

    fn sync(&mut self, _: &usize) -> Result<(), Errno> {
        self.call()?;
        self.synced = self.now.clone();
        self.since.clear();
        Ok(())
    }

    fn rename(&mut self, from: &[u8], to: &[u8]) -> Result<(), Errno> {
        self.call()?;
        self.node(from)?;
        self.change(Change::Rename(from.to_vec(), to.to_vec()));
        Ok(())
    }

    fn unlink(&mut self, path: &[u8]) {
        if self.call().is_ok() && self.now.names.contains_key(path) {
            self.change(Change::Unlink(path.to_vec()));
        }
    }

    fn mkdir(&mut self, path: &[u8]) -> Result<(), Errno> {
        self.call()?;
        if self.now.dirs.contains(path) {
            return Err(Errno::AlreadyExists);
        }
        self.change(Change::Mkdir(path.to_vec()));
        Ok(())
    }
}

/// Interrupt a replace after each of its calls in turn, and read the
/// document as the medium holds it after the crash.
fn after_every_interruption(document: Option<&[u8]>) -> Vec<Option<Vec<u8>>> {
    let mut seen = Vec::new();
    for calls in 0.. {
        let mut medium = Medium::new(document);
        medium.calls_left = calls;
        let finished = replace(&mut medium, DOC, NEW, ParentDir::Existing, TAG).is_ok();
        seen.push(medium.after_crash().read(DOC).map(<[u8]>::to_vec));
        if finished {
            break;
        }
    }
    seen
}

#[test]
fn a_crash_at_any_point_leaves_the_old_bytes_or_the_new() {
    let seen = after_every_interruption(Some(OLD));
    assert!(seen.len() > 5, "the replace was interrupted at every step");
    for doc in &seen {
        assert!(
            doc.as_deref() == Some(OLD) || doc.as_deref() == Some(NEW),
            "{doc:?}"
        );
    }
    assert_eq!(
        seen.last().and_then(Option::as_deref),
        Some(NEW),
        "a finished replace is on the medium"
    );
}

#[test]
fn a_crash_while_creating_leaves_no_document_or_the_whole_one() {
    for doc in after_every_interruption(None) {
        assert!(doc.is_none() || doc.as_deref() == Some(NEW), "{doc:?}");
    }
}

#[test]
fn the_replacement_keeps_the_originals_owner_group_and_mode() {
    let mut medium = Medium::new(Some(OLD));
    replace(&mut medium, DOC, NEW, ParentDir::Existing, TAG).expect("replace");
    let node = medium.now.names[DOC];
    assert_eq!(medium.now.nodes[node].security, OWNER);
    assert_eq!(medium.now.read(DOC), Some(NEW));
}

#[test]
fn a_refused_write_leaves_the_original_and_no_temporary() {
    let mut medium = Medium::new(Some(OLD));
    medium.refuse_write = true;
    assert_eq!(
        replace(&mut medium, DOC, NEW, ParentDir::Existing, TAG),
        Err(Errno::NoSpace)
    );
    assert_eq!(medium.now.read(DOC), Some(OLD));
    assert_eq!(
        medium.now.names.keys().collect::<Vec<_>>(),
        [&DOC.to_vec()],
        "the temporary is gone"
    );
}

#[test]
fn a_missing_directory_is_created_only_when_asked_and_found_missing() {
    let path = b"/Settings/Time/state";
    let mut medium = Medium::new(None);
    assert_eq!(
        replace(&mut medium, path, NEW, ParentDir::Existing, TAG),
        Err(Errno::NotFound)
    );
    replace(&mut medium, path, NEW, ParentDir::CreatedIfMissing, TAG).expect("replace");
    assert_eq!(medium.now.read(path), Some(NEW));
    replace(&mut medium, path, OLD, ParentDir::CreatedIfMissing, TAG)
        .expect("a present directory is not created again");
    assert_eq!(medium.now.read(path), Some(OLD));
}

#[test]
fn the_temporary_is_a_tagged_sibling() {
    assert_eq!(
        staged_path(DOC, TAG).expect("fits"),
        b"/Settings/doc.0f0f0f0f0f0f0f0f.new".to_vec()
    );
    assert_eq!(parent_of(b"/Settings/doc"), Some(&b"/Settings"[..]));
    assert_eq!(parent_of(b"/doc"), Some(&b"/"[..]));
    assert_eq!(parent_of(b"doc"), None);
    assert_ne!(
        staged_path(DOC, [1; TAG_LEN]).expect("fits"),
        staged_path(DOC, [2; TAG_LEN]).expect("fits"),
        "two replaces never share a temporary"
    );
}
