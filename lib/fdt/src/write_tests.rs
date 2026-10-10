//! The writer's blobs read back through the crate's own reader, and every way
//! a tree can be refused.

use alloc::vec::Vec;

use super::{FdtWriteError, FdtWriter};
use crate::Fdt;

fn be(blob: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(blob[at..at + 4].try_into().expect("a word"))
}

/// A root holding a fragment and a second node, sharing property names.
fn overlay_like() -> FdtWriter {
    let mut tree = FdtWriter::new();
    tree.begin_node("");
    tree.prop_strs("compatible", &["vendor,board", "vendor,soc"]);
    tree.begin_node("fragment@0");
    tree.prop_u32("target", 0xFFFF_FFFF);
    tree.begin_node("__overlay__");
    tree.prop_strs("compatible", &["vendor,part"]);
    tree.prop_cells("dmas", &[0xFFFF_FFFF, 1]);
    tree.prop_str("status", "okay");
    tree.end_node();
    tree.end_node();
    tree.end_node();
    tree
}

#[test]
fn a_written_tree_reads_back_as_it_was_written() {
    let blob = overlay_like().finish().expect("well formed");
    let fdt = Fdt::new(&blob).expect("a valid blob");
    let fragment = fdt
        .nodes()
        .filter_map(Result::ok)
        .find(|node| node.name() == b"fragment@0")
        .expect("the fragment");
    assert_eq!(
        fragment.property("target").map(|p| p.value().to_vec()),
        Some(0xFFFF_FFFFu32.to_be_bytes().to_vec())
    );
    let overlay = fdt
        .nodes()
        .filter_map(Result::ok)
        .find(|node| node.name() == b"__overlay__")
        .expect("the overlay");
    assert!(overlay.is_compatible(b"vendor,part"));
    let dmas: Vec<u8> = [0xFFFF_FFFFu32, 1]
        .iter()
        .flat_map(|c| c.to_be_bytes())
        .collect();
    assert_eq!(
        overlay.property("dmas").map(|p| p.value().to_vec()),
        Some(dmas)
    );
    assert_eq!(
        overlay.property("status").map(|p| p.value().to_vec()),
        Some(b"okay\0".to_vec())
    );
}

#[test]
fn the_header_points_at_an_empty_reservation_map_and_names_are_stored_once() {
    let blob = overlay_like().finish().expect("well formed");
    assert_eq!(be(&blob, 0), 0xd00d_feed);
    assert_eq!(be(&blob, 4) as usize, blob.len(), "the total size");
    let reservations = be(&blob, 16) as usize;
    assert_eq!(reservations % 8, 0, "eight-aligned");
    assert!(blob[reservations..reservations + 16]
        .iter()
        .all(|&b| b == 0));
    assert_eq!(be(&blob, 20), 17, "the version");
    assert_eq!(be(&blob, 24), 16, "readable as 16");
    let strings_at = be(&blob, 12) as usize;
    let strings = &blob[strings_at..strings_at + be(&blob, 32) as usize];
    let compatible = strings
        .split(|&b| b == 0)
        .filter(|name| *name == b"compatible")
        .count();
    assert_eq!(compatible, 1, "two compatibles, one name");
    assert_eq!(be(&blob, 8) as usize % 4, 0, "the structure four-aligned");
}

#[test]
fn a_tree_that_does_not_nest_into_one_root_is_refused() {
    let mut open = FdtWriter::new();
    open.begin_node("");
    assert_eq!(open.finish(), Err(FdtWriteError::Unbalanced));

    let mut extra_close = FdtWriter::new();
    extra_close.begin_node("");
    extra_close.end_node();
    extra_close.end_node();
    assert_eq!(extra_close.finish(), Err(FdtWriteError::Unbalanced));

    let mut two_roots = FdtWriter::new();
    for _ in 0..2 {
        two_roots.begin_node("");
        two_roots.end_node();
    }
    assert_eq!(two_roots.finish(), Err(FdtWriteError::Unbalanced));

    let mut loose = FdtWriter::new();
    loose.prop_u32("orphan", 1);
    loose.begin_node("");
    loose.end_node();
    assert_eq!(loose.finish(), Err(FdtWriteError::Unbalanced));

    assert_eq!(
        FdtWriter::new().finish(),
        Err(FdtWriteError::Unbalanced),
        "no root"
    );
}

#[test]
fn a_name_or_string_holding_a_nul_or_an_empty_property_name_is_refused() {
    for build in [
        |tree: &mut FdtWriter| tree.begin_node("a\0b"),
        |tree: &mut FdtWriter| tree.prop_u32("", 1),
        |tree: &mut FdtWriter| tree.prop_u32("a\0b", 1),
        |tree: &mut FdtWriter| tree.prop_strs("list", &["fine", "not\0fine"]),
    ] {
        let mut tree = FdtWriter::new();
        tree.begin_node("");
        build(&mut tree);
        if tree.depth == 2 {
            tree.end_node();
        }
        tree.end_node();
        assert_eq!(tree.finish(), Err(FdtWriteError::BadName));
    }
}
