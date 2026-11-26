use super::NameIndex;
use tairix_hash::HashSeed;

#[test]
fn a_name_is_found_whatever_its_case_and_replaced_or_removed_alike() {
    let mut index = NameIndex::<8>::new(HashSeed::UNKEYED);
    assert!(index.insert(b"Pipe", 1));
    assert_eq!(index.get(b"pIPE"), Some(1));
    assert!(index.insert(b"PIPE", 2));
    assert_eq!(index.get(b"pipe"), Some(2));
    index.remove(b"piPe");
    assert_eq!(index.get(b"Pipe"), None);
}

#[test]
fn a_name_longer_than_the_index_holds_is_neither_kept_nor_found() {
    let mut index = NameIndex::<4>::new(HashSeed::UNKEYED);
    assert!(index.insert(b"four", 4));
    assert!(!index.insert(b"fives", 5));
    assert_eq!(index.get(b"fives"), None);
    assert_eq!(index.get(b"FOUR"), Some(4));
}
