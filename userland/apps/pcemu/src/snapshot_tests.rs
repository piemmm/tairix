use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;

use super::*;

fn saved(part: &impl State) -> Vec<u8> {
    let mut out = Out::refusable();
    part.save(&mut out);
    out.finish().unwrap()
}

fn restored<T: State>(mut into: T, bytes: &[u8]) -> Result<T, Torn> {
    let mut input = In::new(bytes);
    into.restore(&mut input)?;
    check(input.is_exhausted())?;
    Ok(into)
}

#[derive(Debug, Default, PartialEq)]
struct Part {
    count: u16,
    on: bool,
    held: [u8; 3],
    latch: Option<u32>,
    size: u32,
}

state!(Part { count, on, held, latch } as built { size } check |part: &Part| part.count < 1000);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Phase {
    #[default]
    Idle,
    Seek(u8, bool),
    Read {
        sector: u32,
    },
}

state_enum!(Phase {
    0 => Idle,
    1 => Seek(track, head),
    2 => Read { sector },
});

#[test]
fn every_scalar_comes_back_as_it_went() {
    let values = (
        u8::MAX,
        (-2i16, 0x0123_4567_89AB_CDEFu64),
        (-1i64, 1.5f32, usize::MAX),
    );
    let back = restored(Default::default(), &saved(&values)).unwrap();
    assert_eq!(values, back);
}

#[test]
fn a_part_keeps_its_fields_and_leaves_what_it_was_built_with() {
    let part = Part {
        count: 7,
        on: true,
        held: [1, 2, 3],
        latch: Some(9),
        size: 64,
    };
    let built = Part {
        size: 128,
        ..Part::default()
    };
    let back = restored(built, &saved(&part)).unwrap();
    assert_eq!(back, Part { size: 128, ..part });
}

#[test]
fn a_part_its_check_refuses_is_malformed() {
    let part = Part {
        count: 1000,
        ..Part::default()
    };
    assert_eq!(
        restored(Part::default(), &saved(&part)),
        Err(Torn::Malformed)
    );
}

#[test]
fn a_flag_is_nought_or_one_and_a_tag_one_of_the_variants() {
    assert_eq!(restored(false, &[2]), Err(Torn::Malformed));
    assert_eq!(restored(Phase::Idle, &[3]), Err(Torn::Malformed));
    assert_eq!(restored(None::<u8>, &[2, 0]), Err(Torn::Malformed));
    for phase in [
        Phase::Idle,
        Phase::Seek(39, true),
        Phase::Read { sector: 17 },
    ] {
        assert_eq!(restored(Phase::Idle, &saved(&phase)), Ok(phase));
    }
}

#[test]
fn a_snapshot_cut_short_is_malformed() {
    let bytes = saved(&Part::default());
    for len in 0..bytes.len() {
        assert_eq!(
            restored(Part::default(), &bytes[..len]),
            Err(Torn::Malformed),
            "{len}"
        );
    }
}

#[test]
fn a_part_fitted_otherwise_is_a_mismatch() {
    let mut out = Out::refusable();
    save_fitted(Some(&5u8), &mut out);
    let bytes = out.finish().unwrap();
    let mut none: Option<u8> = None;
    assert_eq!(
        restore_fitted(none.as_mut(), &mut In::new(&bytes)),
        Err(Torn::Mismatch)
    );
    let mut some = Some(0u8);
    restore_fitted(some.as_mut(), &mut In::new(&bytes)).unwrap();
    assert_eq!(some, Some(5));
}

#[test]
fn a_buffer_is_taken_back_only_at_the_length_it_was_built() {
    let mut out = Out::refusable();
    save_bytes(&[1, 2, 3, 4], &mut out);
    let bytes = out.finish().unwrap();
    let mut same = [0u8; 4];
    restore_bytes(&mut same, &mut In::new(&bytes)).unwrap();
    assert_eq!(same, [1, 2, 3, 4]);
    for len in [3, 5] {
        let mut other = vec![0u8; len];
        assert_eq!(
            restore_bytes(&mut other, &mut In::new(&bytes)),
            Err(Torn::Mismatch)
        );
    }
    assert_eq!(
        restored(vec![0u16; 2], &saved(&vec![1u16; 3])),
        Err(Torn::Mismatch)
    );
    assert_eq!(
        restored(vec![0u8; 2], &saved(&vec![1u8; 3])),
        Err(Torn::Mismatch)
    );
    assert_eq!(
        restored(vec![0u8; 3], &saved(&vec![1u8; 3])),
        Ok(vec![1u8; 3])
    );
}

#[test]
fn a_queue_holds_no_more_than_its_bound() {
    let queue: VecDeque<u8> = (0..5).collect();
    let mut out = Out::refusable();
    save_queue(&queue, &mut out);
    let bytes = out.finish().unwrap();
    let mut back = VecDeque::from([9u8]);
    restore_queue(&mut back, 5, &mut In::new(&bytes)).unwrap();
    assert_eq!(back, queue);
    assert_eq!(
        restore_queue(&mut back, 4, &mut In::new(&bytes)),
        Err(Torn::Malformed)
    );
}
