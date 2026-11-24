//! Each committed pass run on the software device against the software path
//! it is held to, bit for bit, over pictures drawn from the run's seed: the
//! frost against `Surface::frost_halved_from` and `halved_reference`, the
//! over against `blend_span`, the encode against `ChannelOrder::encode_run`,
//! and the copy and fill against the rows they lay; and the compositor's
//! chrome ([`chrome`]).

#[path = "conformance/chrome.rs"]
mod chrome;
#[path = "conformance/device.rs"]
mod device;
#[path = "conformance/paint.rs"]
mod paint;

use tairix_display::ChannelOrder;
use tairix_fuzzseed::Prng;
use tairix_parallel::Serial;
use tairix_raster::{blend_span, halved_reference, BlurScratch, Blurring, DitherRow, Pixel};
use tairix_shaders::{compose, frost, groups, indirect};

use device::{gpu, picture, pixel, premultiplied, run, word, words, Gpu};

/// The frost's first three passes over `rect` of the `stride`-wide `plane`:
/// the blurred field, `rect`'s width a row.
fn field(
    gpu: &Gpu,
    plane: &[u32],
    stride: u32,
    rect: (u32, u32, u32, u32),
    radius: u32,
) -> Vec<u32> {
    let (left, top, width, height) = rect;
    let reduce = frost::reduce::Push {
        stride,
        left,
        top,
        width,
        height,
    };
    let (across, down) = reduce.cells();
    let cells = vec![0; usize::try_from(across * down).expect("small")];
    let reduced = run(
        gpu,
        frost::reduce::SPIRV,
        &reduce.words(),
        &[plane, &cells],
        groups(across, down),
    );
    let mut blurred = reduced[1].clone();
    for vertical in [false, true] {
        let pass = frost::blur::Push {
            across,
            down,
            reach: radius / 2,
            vertical,
        };
        blurred = run(
            gpu,
            frost::blur::SPIRV,
            &pass.words(),
            &[&blurred, &cells],
            groups(across, down),
        )
        .swap_remove(1);
    }
    let out = vec![0; usize::try_from(width * height).expect("small")];
    let spread = frost::spread::Push {
        stride: width,
        left: 0,
        top: 0,
        width,
        height,
    };
    run(
        gpu,
        frost::spread::SPIRV,
        &spread.words(),
        &[&blurred, &out],
        groups(width, height),
    )
    .swap_remove(1)
}

#[test]
fn the_frost_passes_draw_the_half_resolution_blur_to_the_bit() {
    let gpu = gpu();
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_frost_passes_draw_the_half_resolution_blur_to_the_bit",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    for (width, height, radius) in [
        (1, 1, 5),
        (1, 9, 3),
        (7, 1, 43),
        (13, 11, 7),
        (24, 17, 43),
        (31, 6, 1),
    ] {
        let picture = picture(&mut rng, width, height);
        let expected = halved_reference(
            picture.pixels(),
            usize::try_from(width).expect("small"),
            usize::try_from(height).expect("small"),
            radius,
        );
        let blurred = field(&gpu, &words(&picture), width, (0, 0, width, height), radius);
        let blurred: Vec<Pixel> = blurred.into_iter().map(pixel).collect();
        assert!(blurred == expected, "{width} x {height} at radius {radius}");
    }
}

#[test]
fn the_spread_lays_its_rectangle_at_its_place_in_a_surface_and_nowhere_else() {
    let gpu = gpu();
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_spread_lays_its_rectangle_at_its_place_in_a_surface_and_nowhere_else",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let (width, height, radius) = (15, 10, 9);
    let picture = picture(&mut rng, width, height);
    let blurred = field(&gpu, &words(&picture), width, (0, 0, width, height), radius);
    let (surface_w, surface_h, left, top) = (26, 19, 5, 7);
    let reduce = frost::reduce::Push {
        stride: width,
        left: 0,
        top: 0,
        width,
        height,
    };
    let (across, down) = reduce.cells();
    let reduced = run(
        &gpu,
        frost::reduce::SPIRV,
        &reduce.words(),
        &[
            &words(&picture),
            &vec![0; usize::try_from(across * down).expect("small")],
        ],
        groups(across, down),
    );
    let mut cells = reduced[1].clone();
    for vertical in [false, true] {
        let pass = frost::blur::Push {
            across,
            down,
            reach: radius / 2,
            vertical,
        };
        cells = run(
            &gpu,
            frost::blur::SPIRV,
            &pass.words(),
            &[&cells, &cells],
            groups(across, down),
        )
        .swap_remove(1);
    }
    let surface: Vec<u32> = (0..surface_w * surface_h).map(|_| rng.next_u32()).collect();
    let spread = frost::spread::Push {
        stride: surface_w,
        left,
        top,
        width,
        height,
    };
    let laid = run(
        &gpu,
        frost::spread::SPIRV,
        &spread.words(),
        &[&cells, &surface],
        groups(width, height),
    )
    .swap_remove(1);
    for y in 0..surface_h {
        for x in 0..surface_w {
            let at = usize::try_from(y * surface_w + x).expect("small");
            let inside = (left..left + width).contains(&x) && (top..top + height).contains(&y);
            let expected = if inside {
                blurred[usize::try_from((y - top) * width + (x - left)).expect("small")]
            } else {
                surface[at]
            };
            assert_eq!(laid[at], expected, "({x}, {y})");
        }
    }
}

#[test]
fn the_frost_mixed_over_a_surface_is_the_software_frost_to_the_bit() {
    let gpu = gpu();
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_frost_mixed_over_a_surface_is_the_software_frost_to_the_bit",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let (screen_w, screen_h) = (37, 29);
    for (rect, radius) in [
        ((3, 2, 21, 19), 9),
        ((0, 0, 37, 29), 43),
        ((30, 25, 7, 4), 5),
        ((11, 0, 1, 29), 3),
    ] {
        let backdrop = picture(&mut rng, screen_w, screen_h);
        let mut software = picture(&mut rng, screen_w, screen_h);
        let surface = words(&software);
        let (left, top, width, height) = rect;
        let coverage: Vec<u8> = (0..width * height).map(|_| rng.edge_biased_u8()).collect();
        let mut plane = backdrop.clone();
        let bands = [(left..left + width, top..top + height)];
        let frosted = software.frost_halved_from(
            &mut plane,
            &Blurring {
                rect,
                bands: &bands,
                radius,
            },
            &mut BlurScratch::new(),
            &Serial,
            |x, y| coverage[usize::try_from((y - top) * width + (x - left)).expect("small")],
        );
        assert!(frosted, "the software frost was written");

        let blurred = field(&gpu, &words(&backdrop), screen_w, rect, radius);
        let mut packed = vec![0u32; coverage.len().div_ceil(4)];
        for (at, level) in coverage.iter().enumerate() {
            packed[at / 4] |= u32::from(*level) << ((at % 4) * 8);
        }
        let mix = frost::mix::Push {
            stride: screen_w,
            left,
            top,
            width,
            height,
            blur_stride: width,
            blur_left: 0,
            blur_top: 0,
            coverage_stride: width,
            dithered: true,
        };
        let mixed = run(
            &gpu,
            frost::mix::SPIRV,
            &mix.words(),
            &[&surface, &blurred, &packed],
            groups(width, height),
        );
        let mixed: Vec<Pixel> = mixed[0].iter().copied().map(pixel).collect();
        assert!(mixed == software.pixels(), "{rect:?} at radius {radius}");
        assert!(
            words(&software) != surface,
            "{rect:?}: the frost changed nothing"
        );
    }
}

#[test]
fn the_over_pass_is_the_span_composite_to_the_bit() {
    let gpu = gpu();
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_over_pass_is_the_span_composite_to_the_bit",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let (screen_w, screen_h) = (29, 21);
    for (rect, factor, dithered) in [
        ((0, 0, 29, 21), 255, true),
        ((5, 3, 17, 12), 128, true),
        ((13, 9, 16, 12), 77, false),
        ((0, 20, 29, 1), 0, true),
    ] {
        let (left, top, width, height) = rect;
        let mut software = picture(&mut rng, screen_w, screen_h);
        let target = words(&software);
        let (source_w, source_h) = (width + 3, height + 2);
        let source: Vec<Pixel> = (0..source_w * source_h)
            .map(|_| premultiplied(&mut rng))
            .collect();
        let (source_left, source_top) = (2, 1);
        for row in 0..height {
            let y = top + row;
            let start = usize::try_from(y * screen_w + left).expect("small");
            let dst =
                &mut software.pixels_mut()[start..start + usize::try_from(width).expect("small")];
            let from = usize::try_from((source_top + row) * source_w + source_left).expect("small");
            let src = &source[from..from + usize::try_from(width).expect("small")];
            let dither = if dithered {
                DitherRow::at(y)
            } else {
                DitherRow::NEAREST
            };
            blend_span(dst, src, factor, dither, left);
        }
        let over = compose::over::Push {
            stride: screen_w,
            left,
            top,
            source_stride: source_w,
            source_left,
            source_top,
            width,
            height,
            factor,
            dithered,
        };
        let source_words: Vec<u32> = source.iter().copied().map(word).collect();
        let laid = run(
            &gpu,
            compose::over::SPIRV,
            &over.words(),
            &[&target, &source_words],
            groups(width, height),
        );
        let laid: Vec<Pixel> = laid[0].iter().copied().map(pixel).collect();
        assert!(
            laid == software.pixels(),
            "{rect:?} at {factor}, dithered {dithered}"
        );
        assert_eq!(
            words(&software) == target,
            factor == 0,
            "{rect:?}: only a factor of nought lays nothing"
        );
    }
}

#[test]
fn the_copy_and_fill_passes_lay_their_rectangles_and_nothing_else() {
    let gpu = gpu();
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_copy_and_fill_passes_lay_their_rectangles_and_nothing_else",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let (screen_w, screen_h) = (23, 17);
    let (left, top, width, height) = (4, 3, 11, 9);
    let target: Vec<u32> = (0..screen_w * screen_h).map(|_| rng.next_u32()).collect();
    let source: Vec<u32> = (0..20 * 20).map(|_| rng.next_u32()).collect();
    let copied = run(
        &gpu,
        compose::copy::SPIRV,
        &compose::copy::Push {
            stride: screen_w,
            left,
            top,
            source_stride: 20,
            source_left: 6,
            source_top: 5,
            width,
            height,
        }
        .words(),
        &[&target, &source],
        groups(width, height),
    );
    let pixel = rng.next_u32();
    let filled = run(
        &gpu,
        compose::fill::SPIRV,
        &compose::fill::Push {
            stride: screen_w,
            left,
            top,
            width,
            height,
            pixel,
        }
        .words(),
        &[&target],
        groups(width, height),
    );
    for y in 0..screen_h {
        for x in 0..screen_w {
            let at = usize::try_from(y * screen_w + x).expect("small");
            let inside = (left..left + width).contains(&x) && (top..top + height).contains(&y);
            let (copy, fill) = if inside {
                let from = usize::try_from((y - top + 5) * 20 + (x - left + 6)).expect("small");
                (source[from], pixel)
            } else {
                (target[at], target[at])
            };
            assert_eq!(copied[0][at], copy, "copied ({x}, {y})");
            assert_eq!(filled[0][at], fill, "filled ({x}, {y})");
        }
    }
}

#[test]
fn the_encode_pass_writes_each_display_order_as_the_display_does() {
    let gpu = gpu();
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_encode_pass_writes_each_display_order_as_the_display_does",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let (width, height) = (19, 7);
    let pixels: Vec<Pixel> = (0..width * height)
        .map(|_| premultiplied(&mut rng))
        .collect();
    let source: Vec<u32> = pixels.iter().copied().map(word).collect();
    for order in [ChannelOrder::Rgba, ChannelOrder::Bgra] {
        let mut expected = vec![0u8; pixels.len() * 4];
        assert_eq!(order.encode_run(&pixels, &mut expected), pixels.len());
        let probe = order.encode(Pixel {
            r: 0,
            g: 1,
            b: 2,
            a: 3,
        });
        let frame = vec![0u32; pixels.len()];
        let encoded = run(
            &gpu,
            compose::encode::SPIRV,
            &compose::encode::Push {
                stride: width,
                left: 0,
                top: 0,
                source_stride: width,
                source_left: 0,
                source_top: 0,
                width,
                height,
                order: u32::from_le_bytes(probe),
            }
            .words(),
            &[&frame, &source],
            groups(width, height),
        );
        let bytes: Vec<u8> = encoded[0]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        assert_eq!(bytes, expected, "{order:?}");
    }
}

/// The counts the check leaves in a slot, and its fault, for `counts` held
/// to `most` with the slot's fault `fault` before: past the device's on any
/// axis, every axis zeroed and the fault set; else both kept.
fn checked(counts: [u32; 3], most: [u32; 3], fault: u32) -> ([u32; 3], u32) {
    if counts.iter().zip(most).any(|(&count, most)| count > most) {
        ([0; 3], 1)
    } else {
        (counts, fault)
    }
}

#[test]
fn the_indirect_counts_check_keeps_counts_within_the_device_s_and_zeroes_any_past_them() {
    use indirect::counts;

    let gpu = gpu();
    let mut rng = device::rng(
        "the_indirect_counts_check_keeps_counts_within_the_device_s_and_zeroes_any_past_them",
    );
    let slots = 6u32;
    let most = [65535, 512, 64];
    let near = |rng: &mut Prng, most: u32| match rng.below(4) {
        0 => most,
        1 => most.saturating_add(1),
        2 => rng.next_u32(),
        _ => rng.next_u32() % most.saturating_add(1),
    };
    let mut cases = vec![
        ([65535, 512, 64], 0),
        ([65536, 1, 1], 0),
        ([1, 513, 1], 1),
        ([0, 0, u32::MAX], 0),
        ([0, 0, 0], 1),
    ];
    for _ in 0..24 {
        let counts = [
            near(&mut rng, most[0]),
            near(&mut rng, most[1]),
            near(&mut rng, most[2]),
        ];
        cases.push((counts, rng.next_u32() & 1));
    }
    for (counts, fault) in cases {
        let slot = u32::try_from(rng.below(slots as usize)).expect("a slot");
        let mut held: Vec<u32> = (0..slots * counts::SLOT_WORDS)
            .map(|_| rng.next_u32())
            .collect();
        let mut faults: Vec<u32> = (0..slots).map(|_| rng.next_u32() & 1).collect();
        let at = (slot * counts::SLOT_WORDS) as usize;
        held[at..at + 3].copy_from_slice(&counts);
        faults[slot as usize] = fault;
        let ran = run(
            &gpu,
            counts::SPIRV,
            &counts::Push { slot, most }.words(),
            &[&held, &faults],
            [1, 1, 1],
        );
        let (kept, marked) = checked(counts, most, fault);
        let mut expected = held.clone();
        expected[at..at + 3].copy_from_slice(&kept);
        let mut expected_faults = faults.clone();
        expected_faults[slot as usize] = marked;
        assert_eq!(ran[0], expected, "counts {counts:?} in slot {slot}");
        assert_eq!(ran[1], expected_faults, "counts {counts:?} in slot {slot}");
    }
}

#[test]
fn the_indirect_records_check_zeroes_each_record_naming_a_first_instance_and_marks_its_slot() {
    use indirect::records;

    let gpu = gpu();
    let mut rng = device::rng(
        "the_indirect_records_check_zeroes_each_record_naming_a_first_instance_and_marks_its_slot",
    );
    let slots = 4u32;
    for case in 0..24u32 {
        let indexed = case % 2 == 1;
        let words = if indexed { 5 } else { 4 };
        let count = u32::try_from(rng.below(6)).expect("small");
        let first = u32::try_from(rng.below(8)).expect("small");
        let slot = u32::try_from(rng.below(slots as usize)).expect("a slot");
        let mut held: Vec<u32> = (0..first + count * words + 8)
            .map(|_| rng.next_u32())
            .collect();
        // About half the records name no first instance.
        for record in 0..count {
            if rng.below(2) == 0 {
                held[(first + record * words + words - 1) as usize] = 0;
            }
        }
        let faults: Vec<u32> = (0..slots).map(|_| rng.next_u32() & 1).collect();
        let ran = run(
            &gpu,
            records::SPIRV,
            &records::Push {
                slot,
                first,
                count,
                indexed,
            }
            .words(),
            &[&held, &faults],
            [1, 1, 1],
        );
        let mut expected = held.clone();
        let mut named = false;
        for record in 0..count {
            let start = (first + record * words) as usize;
            let end = start + words as usize;
            if expected[end - 1] != 0 {
                expected[start..end].fill(0);
                named = true;
            }
        }
        let mut expected_faults = faults.clone();
        if named {
            expected_faults[slot as usize] = 1;
        }
        assert_eq!(
            ran[0], expected,
            "case {case}: {count} records from {first}"
        );
        assert_eq!(ran[1], expected_faults, "case {case} in slot {slot}");
    }
}

#[test]
fn every_pinned_pass_changes_what_its_sample_gives_it() {
    let gpu = gpu();
    for pinned in tairix_shaders::SET {
        let sample = (pinned.sample)().expect("a sample");
        let buffers: Vec<&[u32]> = sample.buffers.iter().map(Vec::as_slice).collect();
        let ran = run(&gpu, pinned.spirv, &sample.push, &buffers, sample.groups);
        assert_ne!(
            ran, sample.buffers,
            "{} leaves what its sample gives it as it was",
            pinned.name
        );
    }
}
