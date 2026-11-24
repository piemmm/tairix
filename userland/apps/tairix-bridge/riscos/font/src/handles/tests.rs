use super::*;

fn master(identifier: &str, encoding: Option<&str>) -> Master {
    Master::new(
        identifier.as_bytes().to_vec(),
        None,
        encoding.map(|e| e.as_bytes().to_vec()),
        false,
        b"Resources:$.Fonts.".to_vec(),
        b"Resources:$.Fonts.Encodings.Latin1".to_vec(),
        Source::Face,
    )
}

fn size(points: i32) -> Size {
    Size {
        points: (points, points),
        resolution: (90, 90),
        matrix: None,
    }
}

fn uses(handles: &Handles, handle: u32) -> u32 {
    handles.held(handle).map_or(u32::MAX, |held| held.uses)
}

#[test]
fn a_font_takes_a_handle_for_its_size_and_one_for_its_master() {
    let mut handles = Handles::new();
    assert_eq!(
        handles.find(master("Homerton.Medium", Some("latin1")), size(192)),
        Ok(1)
    );
    assert!(matches!(handles.get(2), Some(Slot::Master(_))));
    assert_eq!(
        handles.find(master("homerton.medium", Some("latin1")), size(192)),
        Ok(1)
    );
    assert_eq!((uses(&handles, 1), uses(&handles, 2)), (2, 1));
    assert_eq!(
        handles.find(master("Trinity.Medium", Some("latin1")), size(192)),
        Ok(3)
    );
    assert!(matches!(handles.get(4), Some(Slot::Master(_))));
}

#[test]
fn a_lost_font_stays_cached_and_new_fonts_take_fresh_handles_first() {
    let mut handles = Handles::new();
    let homerton = handles.find(master("Homerton.Medium", Some("latin1")), size(192));
    let trinity = handles.find(master("Trinity.Medium", Some("latin1")), size(192));
    assert_eq!((homerton, trinity), (Ok(1), Ok(3)));
    handles.lose(3);
    assert_eq!((uses(&handles, 3), uses(&handles, 4)), (0, 0));
    assert_eq!(
        handles.find(master("Corpus.Medium", Some("latin1")), size(192)),
        Ok(5)
    );
    assert_eq!(
        handles.find(master("Trinity.Medium", Some("latin1")), size(192)),
        Ok(3)
    );
    assert_eq!((uses(&handles, 3), uses(&handles, 4)), (1, 1));
}

#[test]
fn each_encoding_has_its_own_master_and_each_size_or_matrix_its_own_handle() {
    let mut handles = Handles::new();
    assert_eq!(
        handles.find(master("Trinity.Medium", Some("latin1")), size(192)),
        Ok(1)
    );
    assert_eq!(
        handles.find(master("Trinity.Medium", Some("latin2")), size(192)),
        Ok(3)
    );
    let mut transformed = size(192);
    transformed.matrix = Some(Matrix([1, 0, 0, 1, 0, 0]));
    assert_eq!(
        handles.find(master("Trinity.Medium", Some("latin1")), transformed),
        Ok(5)
    );
    assert_eq!(uses(&handles, 2), 2);
    assert_eq!(handles.find(master("WIMPSymbol", None), size(192)), Ok(6));
}

#[test]
fn losing_a_handle_counts_down_what_it_holds_and_lets_any_other_be() {
    let mut handles = Handles::new();
    let _ = handles.find(master("Homerton.Medium", Some("latin1")), size(192));
    for handle in [0, 256, u32::MAX, 3] {
        handles.lose(handle);
    }
    assert_eq!((uses(&handles, 1), uses(&handles, 2)), (1, 1));
    handles.lose(2);
    assert_eq!(
        (uses(&handles, 1), uses(&handles, 2)),
        (1, 0),
        "a master counts down too"
    );
    handles.lose(1);
    handles.lose(1);
    assert_eq!((uses(&handles, 1), uses(&handles, 2)), (0, 0));
}

#[test]
fn with_every_handle_taken_cached_fonts_are_reclaimed_least_recently_found_first() {
    let mut handles = Handles::new();
    for points in 0..254 {
        assert!(handles
            .find(master("Trinity.Medium", Some("latin1")), size(100 + points))
            .is_ok());
    }
    assert_eq!(
        handles.find(master("Trinity.Medium", Some("latin1")), size(999)),
        Err(Full)
    );
    handles.lose(1);
    assert_eq!(
        handles.find(master("Corpus.Medium", Some("latin1")), size(192)),
        Err(Full),
        "a new master needs two handles, and only one can be reclaimed"
    );
    assert!(
        matches!(handles.get(1), Some(Slot::Sized(_))),
        "a refusal reclaims nothing"
    );
    handles.lose(3);
    handles.lose(4);
    assert_eq!(
        handles.find(master("Corpus.Medium", Some("latin1")), size(192)),
        Ok(1)
    );
    assert!(matches!(handles.get(3), Some(Slot::Master(_))));
    assert_eq!(
        handles.find(master("Trinity.Medium", Some("latin1")), size(999)),
        Ok(4)
    );
}

#[test]
fn a_master_nothing_refers_to_is_reclaimed_with_its_last_size() {
    let mut handles = Handles::new();
    let _ = handles.find(master("Homerton.Medium", Some("latin1")), size(192));
    handles.lose(1);
    for points in 0..252 {
        assert!(handles
            .find(master("Trinity.Medium", Some("latin1")), size(100 + points))
            .is_ok());
    }
    assert_eq!(
        handles.find(master("Corpus.Medium", Some("latin1")), size(192)),
        Ok(1)
    );
    assert!(matches!(handles.get(2), Some(Slot::Master(m)) if m.identifier == b"Corpus.Medium"));
}
