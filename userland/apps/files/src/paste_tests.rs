//! Host tests for a paste ([`crate::paste`]): carried out against the
//! in-memory tree, a name already taken asked about and answered as its user
//! would, and what the desktop is told of each item.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::window_ipc::{LandingName, LandingOutcome, LandingStep};
use tairix_abi::{Errno, FileId};
use tairix_browse::{plan_paste, Clipboard, ClipboardOp, Listed};
use tairix_fscopy::fixture::{staged, Tree};
use tairix_fscopy::op::{Conflict, Decision};
use tairix_fscopy::tree::Kind;

use crate::landing::{DropLanding, LandingSink};
use crate::operations::Step;
use crate::paste::Paste;

/// Where a drop's steps are recorded.
struct Steps(Rc<RefCell<Vec<LandingStep>>>);

impl LandingSink for Steps {
    fn step(&mut self, step: &LandingStep) -> Result<(), Errno> {
        self.0.borrow_mut().push(*step);
        Ok(())
    }

    fn owe_end(&mut self) {}
}

/// The steps a paste sent the desktop, as they are recorded.
type Sent = Rc<RefCell<Vec<LandingStep>>>;

/// `spelled`'s components, root first.
fn path(spelled: &str) -> Vec<String> {
    spelled
        .split('/')
        .filter(|part| !part.is_empty())
        .map(String::from)
        .collect()
}

/// A paste of `sources`, each as the tree lists it, into `into`, the drop it
/// lands recorded.
fn pasting(
    tree: &Tree,
    op: ClipboardOp,
    sources: &[&str],
    into: &str,
) -> (Sent, Paste<Tree, Steps>) {
    let listed = sources
        .iter()
        .map(|source| Listed {
            path: path(source),
            id: tree.id(source).unwrap_or(FileId::NONE),
        })
        .collect();
    let clipboard = Clipboard::new(op, listed).expect("a clipboard");
    let plan = plan_paste(&clipboard, &path(into)).expect("a plan");
    let sent = Rc::new(RefCell::new(Vec::new()));
    let steps = DropLanding::to(Steps(Rc::clone(&sent)));
    let paste = Paste::new(
        tree.clone(),
        staged,
        (op, path(into), plan.items().to_vec()),
        steps,
    );
    (sent, paste)
}

/// Carry `paste` on until it ends or asks.
fn run(paste: &mut Paste<Tree, Steps>) -> Step {
    let mut current = String::new();
    loop {
        match paste.unit(&mut current) {
            Step::Working => {}
            step => return step,
        }
    }
}

fn making(item: u32, name: &str) -> LandingStep {
    LandingStep::Making {
        item,
        name: LandingName::new(name).expect("a valid name"),
    }
}

const fn made(item: u32, outcome: LandingOutcome) -> LandingStep {
    LandingStep::Made { item, outcome }
}

/// The question about `source` meeting `target`, held by `occupant`.
fn taken(source: &str, target: &str, occupant: Kind) -> Step {
    Step::Asks(Conflict {
        source: String::from(source),
        target: String::from(target),
        occupant,
    })
}

/// A paste onto a name already taken asks, writing nothing meanwhile, where
/// it used to stop; replaced, the desktop is told so.
#[test]
fn a_paste_onto_a_taken_name_asks_instead_of_stopping() {
    let tree = Tree::new()
        .file("/src/notes.txt", b"new")
        .file("/Desktop/notes.txt", b"old");
    let (sent, mut paste) = pasting(&tree, ClipboardOp::Copy, &["/src/notes.txt"], "/Desktop");
    assert_eq!(
        run(&mut paste),
        taken("/src/notes.txt", "/Desktop/notes.txt", Kind::File)
    );
    assert_eq!(
        tree.bytes("/Desktop/notes.txt").as_deref(),
        Some(&b"old"[..]),
        "nothing is written while it asks"
    );
    paste.answer(Decision::Overwrite, false);
    assert_eq!(run(&mut paste), Step::Done);
    assert_eq!(
        tree.bytes("/Desktop/notes.txt").as_deref(),
        Some(&b"new"[..])
    );
    assert_eq!(
        *sent.borrow(),
        [
            making(0, "notes.txt"),
            made(0, LandingOutcome::Replaced),
            LandingStep::Ended,
        ]
    );
}

/// A move onto a name already taken asks too, and a skip leaves both where
/// they were.
#[test]
fn a_move_onto_a_taken_name_asks_and_a_skip_leaves_both() {
    let tree = Tree::new()
        .file("/src/notes.txt", b"mine")
        .file("/Desktop/notes.txt", b"theirs");
    let (sent, mut paste) = pasting(&tree, ClipboardOp::Cut, &["/src/notes.txt"], "/Desktop");
    assert_eq!(
        run(&mut paste),
        taken("/src/notes.txt", "/Desktop/notes.txt", Kind::File)
    );
    paste.answer(Decision::Skip, false);
    assert_eq!(run(&mut paste), Step::Done);
    assert_eq!(tree.bytes("/src/notes.txt").as_deref(), Some(&b"mine"[..]));
    assert_eq!(
        tree.bytes("/Desktop/notes.txt").as_deref(),
        Some(&b"theirs"[..])
    );
    assert_eq!(
        *sent.borrow(),
        [
            making(0, "notes.txt"),
            made(0, LandingOutcome::Skipped),
            LandingStep::Ended,
        ]
    );
}

/// An answer given for the rest is every later question's, so none is asked
/// again.
#[test]
fn an_answer_for_the_rest_asks_no_more() {
    let tree = Tree::new()
        .file("/src/a.txt", b"a")
        .file("/src/b.txt", b"b")
        .file("/src/c.txt", b"c")
        .file("/Desktop/a.txt", b"old a")
        .file("/Desktop/b.txt", b"old b");
    let sources = ["/src/a.txt", "/src/b.txt", "/src/c.txt"];
    let (sent, mut paste) = pasting(&tree, ClipboardOp::Copy, &sources, "/Desktop");
    assert_eq!(
        run(&mut paste),
        taken("/src/a.txt", "/Desktop/a.txt", Kind::File)
    );
    paste.answer(Decision::Skip, true);
    assert_eq!(run(&mut paste), Step::Done, "asked no more");
    assert_eq!(tree.bytes("/Desktop/a.txt").as_deref(), Some(&b"old a"[..]));
    assert_eq!(tree.bytes("/Desktop/b.txt").as_deref(), Some(&b"old b"[..]));
    assert_eq!(tree.bytes("/Desktop/c.txt").as_deref(), Some(&b"c"[..]));
    assert_eq!(
        *sent.borrow(),
        [
            making(0, "a.txt"),
            made(0, LandingOutcome::Skipped),
            making(1, "b.txt"),
            made(1, LandingOutcome::Skipped),
            making(2, "c.txt"),
            made(2, LandingOutcome::Created),
            LandingStep::Ended,
        ]
    );
}

/// A folder meeting a folder of its name is asked about and merges into it,
/// asking about each file inside that is already there — even where the merge
/// was given for the rest, which answers folders alone.
#[test]
fn a_folder_merges_asking_about_each_taken_file_inside() {
    let build = || {
        Tree::new()
            .file("/src/Photos/one.jpg", b"new one")
            .file("/src/Photos/two.jpg", b"two")
            .file("/Desktop/Photos/one.jpg", b"old one")
            .file("/Desktop/Photos/kept.jpg", b"kept")
    };
    let tree = build();
    let (sent, mut paste) = pasting(&tree, ClipboardOp::Copy, &["/src/Photos"], "/Desktop");
    assert_eq!(
        run(&mut paste),
        taken("/src/Photos", "/Desktop/Photos", Kind::Directory)
    );
    paste.answer(Decision::Overwrite, false);
    assert_eq!(
        run(&mut paste),
        taken("/src/Photos/one.jpg", "/Desktop/Photos/one.jpg", Kind::File)
    );
    paste.answer(Decision::Skip, false);
    assert_eq!(run(&mut paste), Step::Done);
    assert_eq!(
        tree.bytes("/Desktop/Photos/one.jpg").as_deref(),
        Some(&b"old one"[..])
    );
    assert_eq!(
        tree.bytes("/Desktop/Photos/two.jpg").as_deref(),
        Some(&b"two"[..])
    );
    assert_eq!(
        tree.bytes("/Desktop/Photos/kept.jpg").as_deref(),
        Some(&b"kept"[..])
    );
    assert_eq!(
        *sent.borrow(),
        [
            making(0, "Photos"),
            made(0, LandingOutcome::Merged),
            LandingStep::Ended,
        ]
    );

    let tree = build();
    let (_, mut paste) = pasting(&tree, ClipboardOp::Copy, &["/src/Photos"], "/Desktop");
    assert_eq!(
        run(&mut paste),
        taken("/src/Photos", "/Desktop/Photos", Kind::Directory)
    );
    paste.answer(Decision::Overwrite, true);
    assert_eq!(
        run(&mut paste),
        taken("/src/Photos/one.jpg", "/Desktop/Photos/one.jpg", Kind::File),
        "a merge given for the rest answered for a file inside"
    );
    paste.answer(Decision::Skip, false);
    assert_eq!(run(&mut paste), Step::Done);
    assert_eq!(
        tree.bytes("/Desktop/Photos/one.jpg").as_deref(),
        Some(&b"old one"[..])
    );
}

/// An answer given for the rest answers only the later questions of its own
/// kind: a merge every later folder's, a skip every later file's, neither the
/// other's.
#[test]
fn an_answer_for_the_rest_answers_its_own_kind_alone() {
    let tree = Tree::new()
        .file("/src/Photos/one.jpg", b"new one")
        .file("/src/Music/song.mp3", b"new song")
        .file("/src/notes.txt", b"new notes")
        .file("/src/todo.txt", b"new todo")
        .file("/Desktop/Photos/one.jpg", b"old one")
        .dir("/Desktop/Music")
        .file("/Desktop/notes.txt", b"old notes")
        .file("/Desktop/todo.txt", b"old todo");
    let sources = [
        "/src/Photos",
        "/src/Music",
        "/src/notes.txt",
        "/src/todo.txt",
    ];
    let (_, mut paste) = pasting(&tree, ClipboardOp::Copy, &sources, "/Desktop");
    assert_eq!(
        run(&mut paste),
        taken("/src/Photos", "/Desktop/Photos", Kind::Directory)
    );
    paste.answer(Decision::Overwrite, true);
    assert_eq!(
        run(&mut paste),
        taken("/src/Photos/one.jpg", "/Desktop/Photos/one.jpg", Kind::File),
        "a merge given for the rest answered for a file"
    );
    paste.answer(Decision::Skip, true);
    assert_eq!(
        run(&mut paste),
        Step::Done,
        "asked again of a kind answered"
    );
    assert_eq!(
        tree.bytes("/Desktop/Music/song.mp3").as_deref(),
        Some(&b"new song"[..]),
        "the later folder was not merged"
    );
    for (path, kept) in [
        ("/Desktop/Photos/one.jpg", &b"old one"[..]),
        ("/Desktop/notes.txt", &b"old notes"[..]),
        ("/Desktop/todo.txt", &b"old todo"[..]),
    ] {
        assert_eq!(tree.bytes(path).as_deref(), Some(kept), "{path} replaced");
    }
}

/// The desktop is told what became of each item: made, replaced, merged,
/// skipped, or failed where its transfer stopped.
#[test]
fn the_desktop_is_told_what_became_of_each_item() {
    let tree = Tree::new()
        .file("/src/new.txt", b"n")
        .file("/src/taken.txt", b"t")
        .file("/src/Folder/inside.txt", b"i")
        .file("/src/skipped.txt", b"s")
        .file("/Desktop/taken.txt", b"old")
        .dir("/Desktop/Folder")
        .file("/Desktop/skipped.txt", b"old");
    let sources = [
        "/src/new.txt",
        "/src/taken.txt",
        "/src/Folder",
        "/src/skipped.txt",
    ];
    let (sent, mut paste) = pasting(&tree, ClipboardOp::Copy, &sources, "/Desktop");
    for answer in [Decision::Overwrite, Decision::Overwrite, Decision::Skip] {
        assert!(matches!(run(&mut paste), Step::Asks(_)));
        paste.answer(answer, false);
    }
    assert_eq!(run(&mut paste), Step::Done);
    assert_eq!(
        tree.bytes("/Desktop/Folder/inside.txt").as_deref(),
        Some(&b"i"[..])
    );
    assert_eq!(
        *sent.borrow(),
        [
            making(0, "new.txt"),
            made(0, LandingOutcome::Created),
            making(1, "taken.txt"),
            made(1, LandingOutcome::Replaced),
            making(2, "Folder"),
            made(2, LandingOutcome::Merged),
            making(3, "skipped.txt"),
            made(3, LandingOutcome::Skipped),
            LandingStep::Ended,
        ]
    );

    let tree = Tree::new().file("/src/gone.txt", b"g").dir("/Desktop");
    let (sent, mut paste) = pasting(&tree, ClipboardOp::Copy, &["/src/gone.txt"], "/Desktop");
    tree.remove("/src/gone.txt");
    assert!(matches!(run(&mut paste), Step::Stopped(_)));
    assert_eq!(
        *sent.borrow(),
        [
            making(0, "gone.txt"),
            made(0, LandingOutcome::Failed),
            LandingStep::Ended,
        ]
    );
}

/// Stopped while it asks, a paste writes nothing more and ends its drop with
/// the item asked about failed.
#[test]
fn a_paste_stopped_while_it_asks_ends_its_drop() {
    let tree = Tree::new()
        .file("/src/notes.txt", b"new")
        .file("/Desktop/notes.txt", b"old");
    let (sent, mut paste) = pasting(&tree, ClipboardOp::Copy, &["/src/notes.txt"], "/Desktop");
    assert!(matches!(run(&mut paste), Step::Asks(_)));
    paste.abandon();
    assert_eq!(
        tree.bytes("/Desktop/notes.txt").as_deref(),
        Some(&b"old"[..])
    );
    assert_eq!(
        *sent.borrow(),
        [
            making(0, "notes.txt"),
            made(0, LandingOutcome::Failed),
            LandingStep::Ended,
        ]
    );
}

/// Stopped part-way through a replacement, a paste removes the replacement it
/// was building and leaves what it would have replaced as it was.
#[test]
fn a_paste_stopped_part_way_through_a_replacement_keeps_what_it_replaces() {
    const STAGED: &str = "/Desktop/notes.txt.staged";
    let tree = Tree::new()
        .file("/src/notes.txt", b"new")
        .file("/Desktop/notes.txt", b"old");
    let (sent, mut paste) = pasting(&tree, ClipboardOp::Copy, &["/src/notes.txt"], "/Desktop");
    assert!(matches!(run(&mut paste), Step::Asks(_)));
    paste.answer(Decision::Overwrite, false);
    let mut current = String::new();
    for _ in 0..8 {
        if tree.exists(STAGED) {
            break;
        }
        assert_eq!(paste.unit(&mut current), Step::Working);
    }
    assert!(tree.exists(STAGED), "the replacement was never staged");
    paste.abandon();
    assert!(!tree.exists(STAGED), "the part-built replacement was left");
    assert_eq!(
        tree.bytes("/Desktop/notes.txt").as_deref(),
        Some(&b"old"[..])
    );
    assert_eq!(
        *sent.borrow(),
        [
            making(0, "notes.txt"),
            made(0, LandingOutcome::Failed),
            LandingStep::Ended,
        ]
    );
}
