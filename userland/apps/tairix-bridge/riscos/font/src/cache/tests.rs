use super::*;

fn master(identifier: &str) -> Master {
    Master::new(
        identifier.as_bytes().to_vec(),
        None,
        Some(b"latin1".to_vec()),
        false,
        b"Resources:$.Fonts.".to_vec(),
        b"Resources:$.Fonts.Encodings.Latin1".to_vec(),
        Source::Face,
    )
}

fn size(points: i32, matrix: Option<Matrix>) -> Size {
    Size {
        points: (points, points),
        resolution: (90, 45),
        matrix,
    }
}

#[test]
fn a_saved_cache_brings_its_fonts_back_cached_on_the_handles_they_had() {
    let mut handles = Handles::new();
    let homerton = handles.find(master("Homerton.Medium"), size(192, None));
    let matrix = Some(Matrix([1, 2, 3, 4, 5, 6]));
    let trinity = handles.find(master("Trinity.Medium"), size(160, matrix));
    assert_eq!((homerton, trinity), (Ok(1), Ok(3)));
    let mut restored = decode(&encode(&handles)).expect("sound");
    assert!(!restored.claimed());
    for handle in 1..=4u32 {
        assert_eq!(
            core::mem::discriminant(restored.get(handle).expect("in range")),
            core::mem::discriminant(handles.get(handle).expect("in range")),
            "handle {handle}"
        );
    }
    assert_eq!(
        restored.find(master("Trinity.Medium"), size(160, matrix)),
        Ok(3)
    );
}

#[test]
fn a_file_that_is_not_one_whole_and_sound_is_refused() {
    let mut handles = Handles::new();
    let _ = handles.find(master("Homerton.Medium"), size(192, None));
    let good = encode(&handles);
    assert!(decode(&good).is_some());
    for cut in 0..good.len() {
        assert!(decode(&good[..cut]).is_none(), "cut at {cut}");
    }
    let mut longer = good.clone();
    longer.push(0);
    assert!(decode(&longer).is_none(), "bytes past its end");
    let mut orphan = MAGIC.to_vec();
    orphan.extend_from_slice(&VERSION.to_le_bytes());
    orphan.extend_from_slice(&1u32.to_le_bytes());
    orphan.extend_from_slice(&[1, SIZED, 2]);
    orphan.extend_from_slice(&[0; 17]);
    assert!(
        decode(&orphan).is_none(),
        "a size whose master is no master"
    );
}

#[test]
fn every_byte_changed_to_every_value_is_read_without_fault() {
    let mut handles = Handles::new();
    let _ = handles.find(master("Homerton.Medium"), size(192, None));
    let good = encode(&handles);
    for at in 0..good.len() {
        for value in [0, 1, 2, 0x7F, 0x80, 0xFE, 0xFF] {
            let mut bytes = good.clone();
            bytes[at] = value;
            let _ = decode(&bytes);
        }
    }
}
