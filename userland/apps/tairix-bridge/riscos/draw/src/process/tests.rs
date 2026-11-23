use alloc::vec::Vec;

use tairix_riscos_kernel::os::{Fail, Unwind};

use super::*;
use crate::raster::Window;
use crate::stage::testkit::{parse, Memory};

const PATH: u32 = 0x1000;
const BUFFER: u32 = 0x8000;
const BOX: u32 = 0x9000;
const MATRIX: u32 = 0xA000;

const SURFACE: Surface = Surface {
    eig: (1, 1),
    origin: (0, 0),
    window: Window {
        left: 0,
        bottom: 0,
        right: 639,
        top: 479,
    },
};

/// The words of a path the probes printed, its end element's room 4096.
fn path_words(text: &str) -> Vec<u32> {
    let mut words = Vec::new();
    for element in parse(text) {
        words.push(element.code());
        for point in element.points().into_iter().flatten() {
            words.extend([point.x.cast_unsigned(), point.y.cast_unsigned()]);
        }
    }
    words.extend([0, 4096]);
    words
}

fn memory_with(path: &str) -> Memory {
    let mut memory = Memory::with_words(PATH, &path_words(path));
    memory.put(BUFFER, &[0, 4096]);
    memory.put(MATRIX, &[0x8000, 0, 0, 0x8000, 0, 0]);
    memory
}

fn call(flags: u32, matrix: u32, output: u32) -> Call {
    Call {
        path: PATH,
        flags,
        matrix,
        flatness: 0,
        thickness: 0,
        style: 0,
        dash: 0,
        output,
    }
}

fn run(memory: &mut Memory, call: &Call) -> Result<u32, Stop> {
    process(memory, call, SURFACE, &mut Clip::default(), u64::MAX)
}

/// What is at `at` as a path, up to its end element.
fn path_at(memory: &mut Memory, at: u32) -> Vec<Element> {
    let mut reader = Reader::new(at);
    let mut elements = Vec::new();
    while let Some(read) = reader.next(memory).expect("a path") {
        elements.push(read.element);
    }
    elements
}

const ARCH: &str = "|2 0 0 |6 0 25600 25600 25600 25600 0";

#[test]
fn reserved_and_floating_point_flags_are_refused_before_anything_is_read() {
    for (flags, refusal) in [
        (1 << 24, Refusal::Reserved),
        (1 << 8, Refusal::Reserved),
        (flag::BOUNDS | flag::FLATTEN, Refusal::Reserved),
        (flag::FLOAT, Refusal::Unsupported),
    ] {
        let mut memory = Memory::default();
        assert_eq!(
            run(&mut memory, &call(flags, 0, BUFFER)),
            Err(Stop::Refused(refusal)),
            "{flags:x}"
        );
    }
    let mut memory = memory_with(ARCH);
    assert_eq!(
        run(&mut memory, &call(0x3F | flag::MAKE_CLIP, 0, BUFFER)),
        Ok(BUFFER + 40)
    );
}

#[test]
fn in_place_refuses_any_stage_that_could_change_the_length_and_leaves_the_path() {
    let line = "|2 0 0 |8 9000 0";
    let thick = Call {
        thickness: 1000,
        ..call(flag::THICKEN, 0, 0)
    };
    let dashed = Call {
        dash: 0x2000,
        ..call(0, 0, 0)
    };
    for refused in [
        call(flag::FLATTEN, 0, 0),
        call(flag::CLOSE, 0, 0),
        call(flag::REFLATTEN, 0, 0),
        thick,
        dashed,
    ] {
        let mut memory = memory_with(line);
        assert_eq!(
            run(&mut memory, &refused),
            Err(Stop::Refused(Refusal::MayChangeLength))
        );
        assert_eq!(path_at(&mut memory, PATH), parse(line));
    }
}

#[test]
fn in_place_keeps_client_bits_and_leaves_r0_the_last_points_x_as_risc_os_did() {
    let mut memory = Memory::with_words(
        PATH,
        &[
            0x1234_5602,
            3,
            3,
            0xABCD_EF08,
            (-3i32).cast_unsigned(),
            5,
            0,
            4096,
        ],
    );
    memory.put(MATRIX, &[0x8000, 0, 0, 0x8000, 0, 0]);
    assert_eq!(run(&mut memory, &call(0, MATRIX, 0)), Ok(u32::MAX));
    let words: Vec<u32> = (0..8).map(|i| memory.words[&(PATH + 4 * i)]).collect();
    assert_eq!(
        words,
        [0x1234_5602, 2, 2, 0xABCD_EF08, u32::MAX, 3, 0, 4096]
    );
    for (path, matrix, r0) in [
        ("|2 100 200 |6 1 2 3 4 555 6", 0, 555),
        ("|2 100 200 |7 900 300", 0, 900),
        ("|2 100 200 |8 900 300 |5", 0, 100),
        ("|2 100 200 |8 900 300 |5", MATRIX, 50),
        ("|2 100 200 |8 900 300 |2 33 44 |5", MATRIX, 17),
        ("", 0, 0),
    ] {
        let mut memory = memory_with(path);
        assert_eq!(run(&mut memory, &call(0, matrix, 0)), Ok(r0), "{path}");
    }
    let mut memory = memory_with("|2 0 0 |8 1 1");
    assert_eq!(run(&mut memory, &call(flag::THICKEN, 0, 0)), Ok(1));
    assert_eq!(
        path_at(&mut memory, PATH),
        parse("|3 0 0 |8 1 1"),
        "the thinnest line, in place"
    );
}

#[test]
fn a_box_is_old_style_or_full_word_and_r0_is_as_risc_os_left_it() {
    let mut memory = memory_with(ARCH);
    assert_eq!(run(&mut memory, &call(0, 0, BOX | (1 << 31))), Ok(25600));
    let block: Vec<u32> = (0..4).map(|i| memory.words[&(BOX + 4 * i)]).collect();
    assert_eq!(block, [0, 0, 25600, 25600]);
    let mut memory = memory_with(ARCH);
    let wide = flag::WIDE | flag::BOUNDS | flag::FLATTEN;
    assert_eq!(run(&mut memory, &call(wide, 0, BOX)), Ok(25600));
    assert_eq!(
        memory.words[&(BOX + 12)],
        19200,
        "flattened, the arch is lower"
    );
    let mut memory = memory_with(ARCH);
    memory.put(BOX, &[0x5555_5555; 4]);
    assert_eq!(
        run(&mut memory, &call(flag::WIDE | flag::FLATTEN, 0, BOX)),
        Ok(BOX + 12 + 120)
    );
    let closed = "|2 100 200 |8 900 300 |5";
    for (flags, r0) in [(0, 50), (flag::FLATTEN, 50), (flag::CLOSE, 100)] {
        let mut memory = memory_with(closed);
        let flags = flags | flag::WIDE | flag::BOUNDS;
        assert_eq!(
            run(&mut memory, &call(flags, MATRIX, BOX)),
            Ok(r0),
            "{flags:x}"
        );
    }
}

#[test]
fn a_count_takes_flattening_into_account() {
    let mut memory = memory_with(ARCH);
    assert_eq!(run(&mut memory, &call(flag::FLATTEN, 0, 3)), Ok(140));
    let mut memory = memory_with(ARCH);
    assert_eq!(run(&mut memory, &call(0, 0, 3)), Ok(48));
}

#[test]
fn reflattening_alone_flattens_and_the_thinnest_line_passes_curves() {
    let mut memory = memory_with(ARCH);
    run(&mut memory, &call(flag::REFLATTEN, 0, BUFFER)).expect("reflattened");
    assert_eq!(
        path_at(&mut memory, BUFFER),
        parse(
            "|2 0 0 |8 287 4500 |8 1100 8400 |8 4000 14400 |8 8100 18000 |8 12800 19200 \
             |8 17500 18000 |8 21600 14400 |8 24500 8400 |8 25312 4500 |8 25600 0"
        )
    );
    let mut memory = memory_with(ARCH);
    run(&mut memory, &call(flag::THICKEN, 0, BUFFER)).expect("thin");
    assert_eq!(
        path_at(&mut memory, BUFFER),
        parse("|3 0 0 |6 0 25600 25600 25600 25600 0")
    );
}

#[test]
fn a_fill_needs_flattened_curves_and_answers_as_risc_os_did() {
    let mut memory = memory_with(ARCH);
    assert_eq!(
        run(&mut memory, &call(0x30, 0, 1)),
        Err(Stop::Refused(Refusal::NeedsFlattening))
    );
    let mut memory = memory_with(ARCH);
    assert_eq!(
        run(&mut memory, &call(0x30 | flag::FLATTEN, 0, 1)),
        Ok(PLOTTED)
    );
    assert!(!memory.plotted.is_empty());
    let mut memory = memory_with(ARCH);
    assert_eq!(
        run(&mut memory, &call(0x3C | flag::FLATTEN, 0, 2)),
        Ok(PLOTTED_EXTERIOR)
    );
}

#[test]
fn a_clip_made_is_kept_and_a_fill_within_it_is_held_there() {
    let square = "|2 0 0 |8 5120 0 |8 5120 5120 |8 0 5120 |5";
    let mut memory = memory_with(square);
    let mut clip = Clip::default();
    process(
        &mut memory,
        &call(0x30 | flag::MAKE_CLIP, 0, 1),
        SURFACE,
        &mut clip,
        u64::MAX,
    )
    .expect("made");
    assert!(memory.plotted.is_empty());
    assert_eq!(clip.spans.len(), 10);
    let wide = "|2 0 0 |8 51200 0 |8 51200 2560 |8 0 2560 |5";
    let mut memory = memory_with(wide);
    process(
        &mut memory,
        &call(0x30 | flag::WITHIN_CLIP, 0, 1),
        SURFACE,
        &mut clip,
        u64::MAX,
    )
    .expect("plotted");
    assert_eq!(
        memory.plotted,
        [(0, 0, 9), (1, 0, 9), (2, 0, 9), (3, 0, 9), (4, 0, 9)]
    );
    assert_eq!(clip.spans.len(), 10, "kept for the next");
}

#[test]
fn closing_closes_every_open_subpath_on_the_way_out() {
    let mut memory = memory_with("|2 0 0 |8 9000 0 |2 5000 5000 |3 1 1 |8 2 2 |5");
    run(&mut memory, &call(flag::CLOSE | flag::THICKEN, 0, BUFFER)).expect("closed");
    assert_eq!(
        path_at(&mut memory, BUFFER),
        parse("|3 0 0 |8 9000 0 |5 |3 5000 5000 |5 |3 1 1 |8 2 2 |5")
    );
}

#[test]
fn every_piece_is_a_step_its_host_may_stop_a_path_at() {
    const DASH: u32 = 0xB000;
    let mut memory = memory_with("|2 0 0 |8 100000 0");
    memory.put(DASH, &[0, 1, 1]);
    let dashed = Call {
        dash: DASH,
        ..call(0, 0, 3)
    };
    assert!(run(&mut memory, &dashed).is_ok());
    assert!(
        memory.stepped >= 100_000,
        "a step a piece: {}",
        memory.stepped
    );
    memory.steps = Some(memory.stepped + 1000);
    assert_eq!(
        run(&mut memory, &dashed),
        Err(Stop::Fail(Fail::Unwind(Unwind::Stopped)))
    );
}
