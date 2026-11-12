use super::*;
use alloc::vec;

/// A text screen's frame: black, with a band of grey "characters".
fn frame(width: u32, height: u32) -> Vec<u8> {
    let mut pixels = vec![0u8; (width * height) as usize * PIXEL];
    for (n, pixel) in pixels.as_chunks_mut::<PIXEL>().0.iter_mut().enumerate() {
        let grey = if n % 7 < 3 && (n / width as usize) % 16 < 8 {
            0xAA
        } else {
            0
        };
        pixel.copy_from_slice(&[grey, grey, grey, 0xFF]);
    }
    pixels
}

fn screen(width: u32, height: u32) -> LastScreen {
    LastScreen {
        width,
        height,
        pixels: frame(width, height),
    }
}

fn suspended() -> Suspended {
    Suspended {
        kept: (0..5000u32).map(|n| (n * 7 % 251) as u8).collect(),
        at: Some(-86_400),
    }
}

/// `state`'s header with its checksum made right again after an edit.
fn resealed(mut state: Vec<u8>) -> Vec<u8> {
    let sum = tairix_crc32::checksum(&state[..HEAD - CRC]);
    state[HEAD - CRC..HEAD].copy_from_slice(&sum.to_le_bytes());
    state
}

#[test]
fn a_screen_reads_back_as_kept_and_a_text_screen_keeps_small() {
    let shown = screen(720, 350);
    let state = encode(&shown, None).unwrap();
    assert!(
        state.len() < shown.pixels.len() / 10,
        "{} bytes",
        state.len()
    );
    assert!(state.len() <= most_bytes());
    assert_eq!(decode(&state), Ok((shown, None)));
    assert!(!head(&state).unwrap().suspended());
}

#[test]
fn a_suspended_machine_keeps_its_snapshot_behind_its_screen() {
    let (shown, asleep) = (screen(640, 200), suspended());
    let state = encode(&shown, Some(&asleep)).unwrap();
    let head = head(&state).unwrap();
    assert!(head.suspended());
    assert_eq!(head.size(), state.len());
    assert_eq!(
        super::screen(&head, &state[head.screen_span()]),
        Ok(shown.clone()),
        "the screen reads alone"
    );
    assert_eq!(decode(&state), Ok((shown, Some(asleep))));
    let unknown = Suspended {
        at: None,
        ..suspended()
    };
    let state = encode(&screen(8, 8), Some(&unknown)).unwrap();
    assert_eq!(decode(&state).unwrap().1, Some(unknown), "no time known");
}

#[test]
fn no_frame_a_monitor_cannot_show_nor_an_empty_snapshot_is_kept() {
    let (w, h) = LARGEST_FRAME;
    let wide = LastScreen {
        width: w + 1,
        height: h,
        pixels: Vec::new(),
    };
    assert_eq!(encode(&wide, None), Err(StateError::Malformed));
    let short = LastScreen {
        width: 2,
        height: 2,
        pixels: vec![0; 15],
    };
    assert_eq!(encode(&short, None), Err(StateError::Malformed));
    let nothing = Suspended {
        kept: Vec::new(),
        at: None,
    };
    assert_eq!(
        encode(&screen(8, 8), Some(&nothing)),
        Err(StateError::Malformed)
    );
    assert!(encode(&screen(w, h), None).is_ok(), "the largest is");
}

#[test]
fn every_flipped_bit_and_every_cut_is_caught() {
    let state = encode(&screen(16, 8), Some(&suspended())).unwrap();
    for at in 0..state.len() * 8 {
        let mut flipped = state.clone();
        flipped[at / 8] ^= 1 << (at % 8);
        assert_eq!(decode(&flipped), Err(StateError::Malformed), "bit {at}");
    }
    for len in 0..state.len() {
        assert_eq!(
            decode(&state[..len]),
            Err(StateError::Malformed),
            "{len} bytes"
        );
    }
}

#[test]
fn a_header_naming_what_no_state_holds_is_refused() {
    let state = encode(&screen(16, 8), None).unwrap();
    let mut taller = state.clone();
    taller[8..10].copy_from_slice(&9u16.to_le_bytes());
    assert_eq!(
        decode(&resealed(taller)),
        Err(StateError::Malformed),
        "the pixels make 8 lines"
    );
    let mut huge = state.clone();
    huge[14..18].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(head(&resealed(huge)), Err(StateError::Malformed));
    let mut older = state;
    older[4..6].copy_from_slice(&1u16.to_le_bytes());
    assert_eq!(head(&resealed(older)), Err(StateError::Malformed));
}
