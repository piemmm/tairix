//! The member tree: names, holds, order, and the depth bound held on every
//! way a directory is made or moved.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::NameMatching;
use tairix_abi::DriverError;

use super::{Body, Kept, Tree, Unheld, MAX_DEPTH, ROOT};
use crate::data::Data;
use crate::zip::directory_meta;

fn tree(matching: NameMatching) -> Tree {
    Tree::new(matching, directory_meta())
}

fn dir(tree: &mut Tree, parent: u64, name: &[u8]) -> Result<u64, DriverError> {
    tree.add(
        parent,
        name,
        Body::Dir(BTreeMap::new()),
        directory_meta(),
        None,
    )
}

fn file(tree: &mut Tree, parent: u64, name: &[u8]) -> Result<u64, DriverError> {
    tree.add(
        parent,
        name,
        Body::File(Data::empty()),
        directory_meta(),
        None,
    )
}

/// A chain of `len` directories under `parent`, answering the deepest.
fn chain(tree: &mut Tree, parent: u64, len: usize) -> u64 {
    (0..len).fold(parent, |at, _| {
        dir(tree, at, b"d").expect("within the bound")
    })
}

#[test]
fn a_directory_nests_no_deeper_than_the_bound_and_a_file_may_lie_in_the_deepest() {
    let mut tree = tree(NameMatching::Exact);
    let deepest = chain(&mut tree, ROOT, MAX_DEPTH);
    assert_eq!(
        dir(&mut tree, deepest, b"over"),
        Err(DriverError::LengthOutOfRange)
    );
    let leaf = file(&mut tree, deepest, b"leaf").expect("a file in the deepest directory");
    assert_eq!(tree.path(leaf).expect("a path").len(), MAX_DEPTH + 1);

    let parts: Vec<Vec<u8>> = (0..=MAX_DEPTH).map(|_| b"p".to_vec()).collect();
    let before = tree.len();
    assert_eq!(
        tree.directory(&parts, &directory_meta()),
        Err(DriverError::LengthOutOfRange)
    );
    assert_eq!(tree.len(), before, "nothing is made for a path refused");
    assert!(tree
        .directory(&parts[..MAX_DEPTH], &directory_meta())
        .is_ok());
}

#[test]
fn a_move_that_would_carry_a_directory_past_the_bound_is_refused_and_depths_follow_a_move() {
    let mut tree = tree(NameMatching::Exact);
    let a = dir(&mut tree, ROOT, b"a").unwrap();
    let a_deep = chain(&mut tree, a, 199);
    let a_mid = tree.node(a_deep).unwrap().parent;
    let b = dir(&mut tree, ROOT, b"b").unwrap();
    let b_deep = chain(&mut tree, b, 99);
    let kept = Kept {
        record: 0..1,
        notes: Vec::new(),
    };
    tree.node_mut(b_deep).unwrap().kept = Some(kept);

    // `b` nests a hundred deep, so beneath the two-hundredth it would reach
    // three hundred.
    assert_eq!(
        tree.rename(ROOT, b"b", a_deep, b"b"),
        Err(DriverError::LengthOutOfRange)
    );
    assert_eq!(
        tree.lookup(ROOT, b"b"),
        Ok(b),
        "a refused move changes nothing"
    );
    assert!(tree.node(b_deep).unwrap().kept.is_some());

    let mut shallower = a_mid;
    for _ in 0..43 {
        shallower = tree.node(shallower).unwrap().parent;
    }
    assert_eq!(tree.node(shallower).unwrap().depth, 156);
    let one_deeper = dir(&mut tree, shallower, b"x").unwrap();
    assert_eq!(
        tree.rename(ROOT, b"b", one_deeper, b"b"),
        Err(DriverError::LengthOutOfRange)
    );
    tree.rename(ROOT, b"b", shallower, b"b")
        .expect("beneath the 156th it reaches the bound exactly");
    assert_eq!(tree.node(b_deep).unwrap().depth, MAX_DEPTH);
    assert!(
        tree.node(b_deep).unwrap().kept.is_none(),
        "a record's path moved with it"
    );
    assert_eq!(
        dir(&mut tree, b_deep, b"over"),
        Err(DriverError::LengthOutOfRange)
    );

    // Moved back up, it has room again.
    tree.rename(shallower, b"b", ROOT, b"b").unwrap();
    assert_eq!(tree.node(b_deep).unwrap().depth, 100);
    assert!(dir(&mut tree, b_deep, b"room").is_ok());
}

#[test]
fn a_directory_is_never_moved_beneath_itself() {
    let mut tree = tree(NameMatching::Exact);
    let a = dir(&mut tree, ROOT, b"a").unwrap();
    let inner = dir(&mut tree, a, b"inner").unwrap();
    assert_eq!(
        tree.rename(ROOT, b"a", inner, b"a"),
        Err(DriverError::DirectoryCycle)
    );
    assert_eq!(
        tree.rename(ROOT, b"a", a, b"self"),
        Err(DriverError::DirectoryCycle)
    );
}

#[test]
fn names_compare_as_the_archive_compares_them() {
    let mut exact = tree(NameMatching::Exact);
    file(&mut exact, ROOT, b"ReadMe").unwrap();
    assert!(file(&mut exact, ROOT, b"README").is_ok());

    let mut folded = tree(NameMatching::AsciiCaseInsensitive);
    let readme = file(&mut folded, ROOT, b"ReadMe").unwrap();
    assert_eq!(
        file(&mut folded, ROOT, b"README"),
        Err(DriverError::AlreadyExists)
    );
    assert_eq!(folded.lookup(ROOT, b"readme"), Ok(readme));
    assert_eq!(
        folded.node(readme).unwrap().name,
        b"ReadMe",
        "the spelling given is kept"
    );
}

#[test]
fn a_held_node_outlives_its_name_and_its_last_release_frees_it() {
    let mut tree = tree(NameMatching::Exact);
    let held = file(&mut tree, ROOT, b"held").unwrap();
    tree.pin(held).unwrap();
    tree.pin(held).unwrap();
    assert_eq!(tree.unlink(ROOT, b"held"), Ok(held));
    assert_eq!(tree.lookup(ROOT, b"held"), Err(DriverError::NotFound));
    assert_eq!(tree.unpin(held), Ok(Unheld::Held));
    assert_eq!(tree.unpin(held), Ok(Unheld::Freed));
    assert!(tree.node(held).is_err());

    let named = file(&mut tree, ROOT, b"named").unwrap();
    tree.pin(named).unwrap();
    assert_eq!(tree.unpin(named), Ok(Unheld::Released));
    assert_eq!(tree.unpin(named), Err(DriverError::NotFound));
}

#[test]
fn a_directory_holding_entries_is_not_unlinked_nor_replaced() {
    let mut tree = tree(NameMatching::Exact);
    let full = dir(&mut tree, ROOT, b"full").unwrap();
    file(&mut tree, full, b"entry").unwrap();
    dir(&mut tree, ROOT, b"empty").unwrap();
    file(&mut tree, ROOT, b"file").unwrap();
    assert_eq!(
        tree.unlink(ROOT, b"full"),
        Err(DriverError::DirectoryNotEmpty)
    );
    assert_eq!(
        tree.rename(ROOT, b"empty", ROOT, b"full"),
        Err(DriverError::DirectoryNotEmpty)
    );
    assert_eq!(
        tree.rename(ROOT, b"file", ROOT, b"empty"),
        Err(DriverError::Unsupported)
    );
    assert_eq!(
        tree.rename(ROOT, b"empty", ROOT, b"file"),
        Err(DriverError::Unsupported)
    );
}

#[test]
fn a_writer_meets_each_directory_before_what_it_holds() {
    let mut tree = tree(NameMatching::Exact);
    let early = file(&mut tree, ROOT, b"early").unwrap();
    let later = dir(&mut tree, ROOT, b"later").unwrap();
    tree.rename(ROOT, b"early", later, b"early").unwrap();
    let order = tree.in_order().unwrap();
    assert_eq!(order, [later, early]);
}
