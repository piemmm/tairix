use super::{replace, replace_link, staged_path, NewLink, ParentDir, ReplaceFs, Security, TAG_LEN};

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::fs::{mode_after_owner_change, FS_NEW_FILE_MODE};
use tairix_abi::{Errno, FileId, FileKind, FileStat, NodeExpect, NodeTimes};

const TAG: [u8; TAG_LEN] = [0x0f; TAG_LEN];
/// The writer's file mode creation mask.
const UMASK: u32 = 0o022;
const DOC: &str = "/Settings/doc";
const OLD: &[u8] = b"old document";
const NEW: &[u8] = b"the new document, longer than the old";
const SOURCE: &str = "/Settings/source";
const SOURCE_BYTES: &[u8] = b"what the hard link names";
const VICTIM: &[u8] = b"what a planted link leads to";

const OWNER: Security = Security {
    uid: 18,
    gid: 18,
    mode: 0o640,
};
/// Another's, as the link a swap plants is.
const PLANTED: Security = Security {
    uid: 7,
    gid: 7,
    mode: 0o604,
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
    /// Kept as data is: on the medium only once synced.
    attrs: BTreeMap<Vec<u8>, Vec<u8>>,
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

    fn attrs(&self, path: &[u8]) -> Option<&BTreeMap<Vec<u8>, Vec<u8>>> {
        self.names.get(path).map(|&node| &self.nodes[node].attrs)
    }
}

/// The attributes the document carries before it is replaced.
fn riscos_attrs() -> BTreeMap<Vec<u8>, Vec<u8>> {
    BTreeMap::from([
        (b"acorn.filetype".to_vec(), b"fff".to_vec()),
        (b"user.origin".to_vec(), b"disc".to_vec()),
    ])
}

/// A name or security change, which a crash may keep without the data the
/// writer meant it to cover.
#[derive(Clone)]
enum Change {
    /// A name for a node, which is born with this security when it is new.
    Create(Vec<u8>, usize, Security),
    Rename(Vec<u8>, Vec<u8>),
    Security(usize, Security),
    Unlink(Vec<u8>),
    Mkdir(Vec<u8>),
}

impl Change {
    fn apply(&self, state: &mut State) {
        match self {
            Self::Create(path, node, security) => {
                if state.nodes.len() <= *node {
                    state.nodes.resize(
                        node + 1,
                        Node {
                            data: Vec::new(),
                            security: *security,
                            attrs: BTreeMap::new(),
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

/// The identity the medium's `stat` reports for its node `node`.
fn id_of(node: usize) -> FileId {
    FileId {
        volume: [1; 16],
        node: node as u64 + 1,
    }
}

/// A medium that keeps only what a sync put on it: after a crash, every name
/// and security change since the last sync may survive while no write since
/// it does, which is the worst a write-back cache can do.
struct Medium {
    now: State,
    synced: State,
    since: Vec<Change>,
    /// Each file created, with the mode it was born with.
    born: Vec<(Vec<u8>, u32)>,
    calls_left: usize,
    fault: Option<Fault>,
}

/// The one way a [`Medium`] misbehaves.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Fault {
    /// Every write is refused for want of space.
    Write,
    /// Every attribute change is refused.
    Attrs,
    /// Every rename is refused.
    Rename,
    /// Another writer moves each temporary aside as it is created and plants
    /// a link to [`VICTIM`] at its name.
    SwapTemporaries,
    /// Every stat once the replace has changed anything is refused.
    Stat,
}

impl Medium {
    fn new(document: Option<&[u8]>) -> Self {
        let mut state = State::default();
        state.dirs.insert(b"/Settings".to_vec());
        if let Some(bytes) = document {
            state.nodes.push(Node {
                data: bytes.to_vec(),
                security: OWNER,
                attrs: riscos_attrs(),
            });
            state.names.insert(DOC.as_bytes().to_vec(), 0);
        }
        Self {
            now: state.clone(),
            synced: state,
            since: Vec::new(),
            born: Vec::new(),
            calls_left: usize::MAX,
            fault: None,
        }
    }

    /// A medium also holding the file `SOURCE`, which a hard link names.
    fn with_source(document: Option<&[u8]>) -> Self {
        let mut medium = Self::new(document);
        for state in [&mut medium.now, &mut medium.synced] {
            state.nodes.push(Node {
                data: SOURCE_BYTES.to_vec(),
                security: OWNER,
                attrs: BTreeMap::new(),
            });
            state
                .names
                .insert(SOURCE.as_bytes().to_vec(), state.nodes.len() - 1);
        }
        medium
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

    fn named(&self, path: &[u8]) -> Result<usize, Errno> {
        self.now.names.get(path).copied().ok_or(Errno::NotFound)
    }

    fn new_node(&mut self, data: &[u8], security: Security) -> usize {
        self.now.nodes.push(Node {
            data: data.to_vec(),
            security,
            attrs: BTreeMap::new(),
        });
        self.now.nodes.len() - 1
    }
}

fn spelled(dir: &str, name: &str) -> Vec<u8> {
    tairix_path::join(dir, name).into_bytes()
}

impl ReplaceFs for Medium {
    type Dir = String;
    type File = usize;

    fn parent(&mut self, path: &str) -> Result<(String, String), Errno> {
        self.call()?;
        let dir = tairix_path::dir_name(path);
        if !self.now.dirs.contains(dir.as_bytes()) {
            return Err(Errno::NotFound);
        }
        Ok((
            String::from(dir),
            String::from(tairix_path::leaf_name(path)),
        ))
    }

    fn mkdir(&mut self, path: &str, _mode: u32) -> Result<(), Errno> {
        self.call()?;
        // Refused as the kernel refuses a spelling it cannot parse.
        tairix_path::parse(path).map_err(|_| Errno::OutOfRange)?;
        if self.now.dirs.contains(path.as_bytes()) {
            return Err(Errno::AlreadyExists);
        }
        self.change(Change::Mkdir(path.as_bytes().to_vec()));
        Ok(())
    }

    fn original(&mut self, dir: &String, name: &str) -> Result<Option<usize>, Errno> {
        self.call()?;
        Ok(self.now.names.get(&spelled(dir, name)).copied())
    }

    fn node(&mut self, dir: &String, name: &str) -> Result<usize, Errno> {
        self.call()?;
        self.named(&spelled(dir, name))
    }

    fn create(&mut self, dir: &String, name: &str, mode: u32) -> Result<usize, Errno> {
        self.call()?;
        let path = spelled(dir, name);
        if self.now.names.contains_key(&path) {
            return Err(Errno::AlreadyExists);
        }
        let node = self.now.nodes.len();
        let born = Security {
            mode: mode & !UMASK,
            ..CREATOR
        };
        self.born.push((path.clone(), born.mode));
        if self.fault != Some(Fault::SwapTemporaries) {
            self.change(Change::Create(path, node, born));
            return Ok(node);
        }
        self.change(Change::Create(path.clone(), node, born));
        let mut aside = path.clone();
        aside.extend_from_slice(b".aside");
        self.change(Change::Rename(path.clone(), aside));
        let victim = self.new_node(VICTIM, PLANTED);
        self.change(Change::Create(path, victim, PLANTED));
        Ok(node)
    }

    fn stat(&mut self, file: &usize) -> Result<FileStat, Errno> {
        self.call()?;
        if self.fault == Some(Fault::Stat) && !self.since.is_empty() {
            return Err(Errno::DeviceFault);
        }
        let node = &self.now.nodes[*file];
        Ok(FileStat {
            kind: FileKind::Regular,
            nlink: 1,
            size: node.data.len() as u64,
            allocated: node.data.len() as u64,
            mode: node.security.mode,
            uid: node.security.uid,
            gid: node.security.gid,
            id: id_of(*file),
            times: NodeTimes::default(),
            content_gen: 0,
        })
    }

    fn carry_attributes(&mut self, from: &usize, to: &usize) -> Result<(), Errno> {
        self.call()?;
        if self.fault == Some(Fault::Attrs) {
            return Err(Errno::PermissionDenied);
        }
        // Set by a writer the temporary's eventual owner may not be.
        let made = self.now.nodes[*to].security;
        if (made.uid, made.gid) != (CREATOR.uid, CREATOR.gid) {
            return Err(Errno::PermissionDenied);
        }
        let attrs = self.now.nodes[*from].attrs.clone();
        self.now.nodes[*to].attrs = attrs;
        Ok(())
    }

    fn set_owner(&mut self, file: &usize, uid: u32, gid: u32) -> Result<(), Errno> {
        self.call()?;
        let mut security = self.now.nodes[*file].security;
        security.uid = uid;
        security.gid = gid;
        // As the kernel has it: an ownership change clears the set-ID bits.
        security.mode = mode_after_owner_change(security.mode, false);
        self.change(Change::Security(*file, security));
        Ok(())
    }

    fn set_mode(&mut self, file: &usize, mode: u32) -> Result<(), Errno> {
        self.call()?;
        let mut security = self.now.nodes[*file].security;
        // Only a node's owner sets its mode, whatever the writer holds.
        if security.uid != CREATOR.uid {
            return Err(Errno::PermissionDenied);
        }
        security.mode = mode;
        self.change(Change::Security(*file, security));
        Ok(())
    }

    fn write_all(&mut self, file: &usize, bytes: &[u8]) -> Result<(), Errno> {
        self.call()?;
        if self.fault == Some(Fault::Write) {
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

    fn rename(
        &mut self,
        dir: &String,
        from: &str,
        to: &str,
        expect: &NodeExpect,
    ) -> Result<(), Errno> {
        self.call()?;
        if self.fault == Some(Fault::Rename) {
            return Err(Errno::PermissionDenied);
        }
        let from = spelled(dir, from);
        if id_of(self.named(&from)?) != expect.id {
            return Err(Errno::Stale);
        }
        self.change(Change::Rename(from, spelled(dir, to)));
        Ok(())
    }

    fn forsake(&mut self, dir: &String, name: &str) {
        let path = spelled(dir, name);
        if self.call().is_ok() && self.now.names.contains_key(&path) {
            self.change(Change::Unlink(path));
        }
    }

    fn unlink(&mut self, dir: &String, name: &str, expect: &NodeExpect) {
        let path = spelled(dir, name);
        let named = self
            .now
            .names
            .get(&path)
            .is_some_and(|&node| id_of(node) == expect.id);
        if self.call().is_ok() && named {
            self.change(Change::Unlink(path));
        }
    }

    fn umask(&mut self) -> u32 {
        UMASK
    }

    /// A symbolic link is a node holding its target; a hard link a second
    /// name for its source's node.
    fn make_link(&mut self, new: NewLink<'_>, dir: &String, at: &str) -> Result<(), Errno> {
        self.call()?;
        let at = spelled(dir, at);
        if self.now.names.contains_key(&at) {
            return Err(Errno::AlreadyExists);
        }
        let node = match new {
            NewLink::Hard { existing, .. } => self.named(existing.as_bytes())?,
            NewLink::Symbolic(target) => self.new_node(target.as_bytes(), CREATOR),
        };
        self.change(Change::Create(at, node, CREATOR));
        Ok(())
    }
}

/// More calls than a replace makes: a sweep that reaches it without one
/// finishing has found a replace that cannot.
const SWEEP_LIMIT: usize = 256;

/// Interrupt a replace after each of its calls in turn, and read the
/// document as the medium holds it after the crash.
fn after_every_interruption(document: Option<&[u8]>) -> Vec<Option<Vec<u8>>> {
    let mut seen = Vec::new();
    for calls in 0..=SWEEP_LIMIT {
        assert!(calls < SWEEP_LIMIT, "the replace never finished");
        let mut medium = Medium::new(document);
        medium.calls_left = calls;
        let finished = replace(
            &mut medium,
            DOC,
            NEW,
            ParentDir::Existing,
            FS_NEW_FILE_MODE,
            TAG,
        )
        .is_ok();
        let crashed = medium.after_crash();
        if crashed.read(DOC.as_bytes()).is_some() && document.is_some() {
            assert_eq!(
                crashed.attrs(DOC.as_bytes()),
                Some(&riscos_attrs()),
                "whichever document survives carries its attributes"
            );
        }
        seen.push(crashed.read(DOC.as_bytes()).map(<[u8]>::to_vec));
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
    replace(
        &mut medium,
        DOC,
        NEW,
        ParentDir::Existing,
        FS_NEW_FILE_MODE,
        TAG,
    )
    .expect("replace");
    let node = medium.now.names[DOC.as_bytes()];
    assert_eq!(medium.now.nodes[node].security, OWNER);
    assert_eq!(medium.now.read(DOC.as_bytes()), Some(NEW));
}

#[test]
fn a_first_creation_takes_the_mode_it_names_less_the_umask() {
    for (asked, born) in [(0o600, 0o600), (FS_NEW_FILE_MODE, 0o644), (0o777, 0o755)] {
        let mut medium = Medium::new(None);
        replace(&mut medium, DOC, NEW, ParentDir::Existing, asked, TAG).expect("replace");
        let node = medium.now.names[DOC.as_bytes()];
        assert_eq!(medium.now.nodes[node].security.mode, born, "{asked:o}");
    }
}

/// The temporary takes the original's attributes and its new bytes before
/// its security is settled, so it is born its writer's alone rather than
/// with any mode a reader could share.
#[test]
fn a_temporary_is_its_writers_alone_while_it_is_built() {
    for document in [Some(OLD), None] {
        let mut medium = Medium::new(document);
        replace(
            &mut medium,
            DOC,
            NEW,
            ParentDir::Existing,
            FS_NEW_FILE_MODE,
            TAG,
        )
        .expect("replace");
        let staged = staged_path(DOC.as_bytes(), TAG).expect("fits");
        assert_eq!(medium.born, [(staged, 0o600)]);
    }
}

/// A temporary whose name another writer takes over is still the one written
/// and set, and is put in place only while its name still holds it: the link
/// planted there is neither written through nor renamed over the document.
#[test]
fn a_temporary_swapped_for_a_link_is_never_written_through_nor_put_in_place() {
    let mut medium = Medium::new(Some(OLD));
    medium.fault = Some(Fault::SwapTemporaries);
    assert_eq!(
        replace(
            &mut medium,
            DOC,
            NEW,
            ParentDir::Existing,
            FS_NEW_FILE_MODE,
            TAG
        ),
        Err(Errno::Stale)
    );
    assert_eq!(medium.now.read(DOC.as_bytes()), Some(OLD));
    let staged = staged_path(DOC.as_bytes(), TAG).expect("fits");
    let planted = medium.now.names[&staged];
    assert_eq!(
        medium.now.nodes[planted].data, VICTIM,
        "not written through"
    );
    assert_eq!(
        medium.now.nodes[planted].security, PLANTED,
        "not given a mode or an owner"
    );
    assert!(
        medium.now.nodes[planted].attrs.is_empty(),
        "given no attribute"
    );
}

#[test]
fn a_temporary_that_cannot_be_read_back_is_not_left_behind() {
    let mut medium = Medium::new(Some(OLD));
    medium.fault = Some(Fault::Stat);
    assert_eq!(
        replace(
            &mut medium,
            DOC,
            NEW,
            ParentDir::Existing,
            FS_NEW_FILE_MODE,
            TAG
        ),
        Err(Errno::DeviceFault)
    );
    let staged = staged_path(DOC.as_bytes(), TAG).expect("fits");
    assert!(!medium.now.names.contains_key(&staged));
    assert_eq!(medium.now.read(DOC.as_bytes()), Some(OLD));
    let mut medium = Medium::new(Some(OLD));
    medium.fault = Some(Fault::Stat);
    assert_eq!(
        replace_link(&mut medium, DOC, NewLink::Symbolic("elsewhere"), TAG),
        Err(Errno::DeviceFault)
    );
    assert!(!medium.now.names.contains_key(&staged));
}

#[test]
fn the_replacement_keeps_the_originals_extended_attributes() {
    let mut medium = Medium::new(Some(OLD));
    replace(
        &mut medium,
        DOC,
        NEW,
        ParentDir::Existing,
        FS_NEW_FILE_MODE,
        TAG,
    )
    .expect("replace");
    assert_eq!(medium.now.attrs(DOC.as_bytes()), Some(&riscos_attrs()));
}

#[test]
fn attributes_the_replacement_cannot_take_leave_the_original_and_no_temporary() {
    let mut medium = Medium::new(Some(OLD));
    medium.fault = Some(Fault::Attrs);
    assert_eq!(
        replace(
            &mut medium,
            DOC,
            NEW,
            ParentDir::Existing,
            FS_NEW_FILE_MODE,
            TAG
        ),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(medium.now.read(DOC.as_bytes()), Some(OLD));
    assert_eq!(medium.now.names.len(), 1, "the temporary is gone");
}

#[test]
fn a_refused_write_leaves_the_original_and_no_temporary() {
    let mut medium = Medium::new(Some(OLD));
    medium.fault = Some(Fault::Write);
    assert_eq!(
        replace(
            &mut medium,
            DOC,
            NEW,
            ParentDir::Existing,
            FS_NEW_FILE_MODE,
            TAG
        ),
        Err(Errno::NoSpace)
    );
    assert_eq!(medium.now.read(DOC.as_bytes()), Some(OLD));
    assert_eq!(
        medium.now.names.keys().collect::<Vec<_>>(),
        [&DOC.as_bytes().to_vec()],
        "the temporary is gone"
    );
}

#[test]
fn a_missing_directory_is_created_only_when_asked_and_found_missing() {
    let path = "/Settings/Time/state";
    let mut medium = Medium::new(None);
    assert_eq!(
        replace(
            &mut medium,
            path,
            NEW,
            ParentDir::Existing,
            FS_NEW_FILE_MODE,
            TAG
        ),
        Err(Errno::NotFound)
    );
    replace(
        &mut medium,
        path,
        NEW,
        ParentDir::CreatedIfMissing,
        FS_NEW_FILE_MODE,
        TAG,
    )
    .expect("replace");
    assert_eq!(medium.now.read(path.as_bytes()), Some(NEW));
    replace(
        &mut medium,
        path,
        OLD,
        ParentDir::CreatedIfMissing,
        FS_NEW_FILE_MODE,
        TAG,
    )
    .expect("a present directory is not created again");
    assert_eq!(medium.now.read(path.as_bytes()), Some(OLD));
}

#[test]
fn a_file_at_an_alias_root_has_the_root_for_its_directory() {
    let path = "Settings:/state";
    let mut medium = Medium::new(None);
    replace(
        &mut medium,
        path,
        NEW,
        ParentDir::CreatedIfMissing,
        FS_NEW_FILE_MODE,
        TAG,
    )
    .expect("replace");
    assert!(medium.now.dirs.contains(&b"Settings:/"[..]));
    assert_eq!(medium.now.read(path.as_bytes()), Some(NEW));
}

#[test]
fn the_temporary_is_a_tagged_sibling() {
    assert_eq!(
        staged_path(DOC.as_bytes(), TAG).expect("fits"),
        b"/Settings/doc.0f0f0f0f0f0f0f0f.new".to_vec()
    );
    assert_ne!(
        staged_path(DOC.as_bytes(), [1; TAG_LEN]).expect("fits"),
        staged_path(DOC.as_bytes(), [2; TAG_LEN]).expect("fits"),
        "two replaces never share a temporary"
    );
}

const HARD: NewLink<'static> = NewLink::Hard {
    existing: SOURCE,
    follow: false,
};

/// The node the document's name leads to after a crash interrupting a link
/// replacing it, at each of its calls in turn.
fn link_after_every_interruption(new: NewLink<'_>) -> Vec<Option<usize>> {
    let mut seen = Vec::new();
    for calls in 0..=SWEEP_LIMIT {
        assert!(calls < SWEEP_LIMIT, "the link replace never finished");
        let mut medium = Medium::with_source(Some(OLD));
        medium.calls_left = calls;
        let finished = replace_link(&mut medium, DOC, new, TAG).is_ok();
        seen.push(medium.after_crash().names.get(DOC.as_bytes()).copied());
        if finished {
            break;
        }
    }
    seen
}

#[test]
fn a_crash_while_a_link_replaces_a_name_leaves_the_old_node_or_the_link() {
    let source = Medium::with_source(Some(OLD)).now.names[SOURCE.as_bytes()];
    let seen = link_after_every_interruption(HARD);
    assert!(seen.len() > 2, "the replace was interrupted at every step");
    for node in &seen {
        assert!(*node == Some(0) || *node == Some(source), "{node:?}");
    }
    assert_eq!(
        seen.last(),
        Some(&Some(source)),
        "a finished replace names the source"
    );
    for node in link_after_every_interruption(NewLink::Symbolic("elsewhere")) {
        assert!(node.is_some(), "the name is never missing");
    }
}

#[test]
fn a_hard_link_whose_source_is_missing_leaves_the_name_untouched() {
    let mut medium = Medium::new(Some(OLD));
    assert_eq!(
        replace_link(&mut medium, DOC, HARD, TAG),
        Err(Errno::NotFound)
    );
    assert_eq!(medium.now.read(DOC.as_bytes()), Some(OLD));
    assert_eq!(medium.now.names.len(), 1, "no temporary was left");
}

#[test]
fn a_refused_rename_leaves_the_name_and_removes_the_temporary() {
    let mut medium = Medium::with_source(Some(OLD));
    medium.fault = Some(Fault::Rename);
    assert_eq!(
        replace_link(&mut medium, DOC, HARD, TAG),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(medium.now.read(DOC.as_bytes()), Some(OLD));
    assert_eq!(medium.now.names.len(), 2, "the temporary is gone");
}

#[test]
fn a_symbolic_link_takes_the_name_storing_its_target() {
    let mut medium = Medium::new(Some(OLD));
    replace_link(&mut medium, DOC, NewLink::Symbolic("../elsewhere"), TAG).expect("replace");
    assert_eq!(medium.now.read(DOC.as_bytes()), Some(&b"../elsewhere"[..]));
    assert_eq!(medium.now.names.len(), 1, "no temporary was left");
}

#[test]
fn a_name_too_long_to_carry_the_tag_is_staged_under_the_tag_alone() {
    let mut long = b"/Settings/".to_vec();
    long.resize(long.len() + tairix_abi::fs::FS_NAME_MAX, b'n');
    assert_eq!(
        staged_path(&long, TAG).expect("fits"),
        b"/Settings/.0f0f0f0f0f0f0f0f.new".to_vec()
    );
    let mut fitting = b"/Settings/".to_vec();
    fitting.resize(fitting.len() + tairix_abi::fs::FS_NAME_MAX - 21, b'n');
    let staged = staged_path(&fitting, TAG).expect("fits");
    assert!(staged.starts_with(&fitting) && staged.ends_with(b".0f0f0f0f0f0f0f0f.new"));
    assert_eq!(
        staged.len() - b"/Settings/".len(),
        tairix_abi::fs::FS_NAME_MAX
    );
}
