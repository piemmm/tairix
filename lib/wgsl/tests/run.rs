//! The front end held to WGSL's values: each construct compiled, admitted by
//! the validator again, and run on the software device, its results checked
//! against what the specification defines — the arithmetic SPIR-V leaves
//! undefined above all.

mod support;

use support::{admitted, bytes_of, with_bindings, words_of, Gpu, BINDINGS, WORDS};
use tairix_gpu::{
    AllocationDesc, BindSet, BindSetEntry, BindSetLayout, Buffer, BufferImageCopy, BufferUsage,
    ClearValue, ColourAttachment, ColourMask, ColourTarget, Context, CullMode, DescriptorType,
    Draw, Extent3d, Filter, Format, FrontFace, ImageDesc, ImageDimension, ImageRange,
    ImageSubresource, ImageUsage, ImageViewDesc, LayoutBinding, Load, Multisample, Origin3d, Pass,
    PushConstants, Rasterization, Rect, RenderPipelineDesc, Resource, SampleType, SamplerDesc,
    SamplerType, StageDesc, Topology, ViewDimension, Viewport,
};
use tairix_spirv::enumerant::{Dim, ImageFormat};
use tairix_spirv::{Access, Admitted, Component, ImageShape, Module, Stages};
use tairix_wgsl::{compile, Options, OverrideKey};

/// Run `body`'s `main`, after the shared bindings, over `groups` workgroups
/// with binding 1 holding `input`, and answer binding 0's first `count`
/// words.
fn run_with(
    body: &str,
    options: &Options<'_>,
    input: &[u32],
    groups: u32,
    count: usize,
) -> Vec<u32> {
    Gpu::new().compute(
        admitted(&with_bindings(body), options),
        input,
        groups,
        count,
    )
}

fn run(body: &str, input: &[u32], count: usize) -> Vec<u32> {
    run_with(body, &Options::default(), input, 1, count)
}

/// `module` guarded, admitted again as the device it was guarded for is
/// handed it.
fn guarded(module: Admitted) -> Admitted {
    let guarded = module.guarded().expect("guarded");
    let module = Module::decode(guarded.admitted().module().bytes()).expect("decoded");
    Admitted::new(module).expect("admitted")
}

fn bits(x: f32) -> u32 {
    x.to_bits()
}

#[test]
fn integer_division_and_remainder_are_wgsls_by_zero_and_by_minus_one() {
    let out = run(
        "
        @compute @workgroup_size(1)
        fn main() {
            let zero = i32(input[0]);
            let minus_one = i32(input[1]);
            let lowest = i32(input[2]);
            let seven = i32(input[3]);
            out[0] = bitcast<u32>(seven / zero);
            out[1] = bitcast<u32>(seven % zero);
            out[2] = bitcast<u32>(lowest / minus_one);
            out[3] = bitcast<u32>(lowest % minus_one);
            out[4] = bitcast<u32>(-seven / 2);
            out[5] = bitcast<u32>(-seven % 2);
            out[6] = input[3] / input[0];
            out[7] = input[3] % input[0];
            let v = vec2i(seven, lowest) / vec2i(zero, minus_one);
            out[8] = bitcast<u32>(v.x);
            out[9] = bitcast<u32>(v.y);
            let w = vec2u(input[3]) % vec2u(input[0], 4u);
            out[10] = w.x;
            out[11] = w.y;
        }
        ",
        &[0, u32::MAX, 0x8000_0000, 7],
        12,
    );
    assert_eq!(
        out,
        [
            7,
            0,
            0x8000_0000,
            0,
            (-3i32).cast_unsigned(),
            (-1i32).cast_unsigned(),
            7,
            0,
            7,
            0x8000_0000,
            0,
            3
        ]
    );
}

#[test]
fn a_shift_takes_its_amount_modulo_the_width() {
    let out = run(
        "
        @compute @workgroup_size(1)
        fn main() {
            let one = input[0];
            let amount = input[1];
            out[0] = one << amount;
            out[1] = 0x80000000u >> amount;
            out[2] = bitcast<u32>(bitcast<i32>(input[2]) >> amount);
            out[3] = one << (amount + 31u);
            let v = vec2u(one, 3u) << vec2u(amount, 32u);
            out[4] = v.x;
            out[5] = v.y;
        }
        ",
        &[1, 33, 0x8000_0000],
        6,
    );
    assert_eq!(out, [2, 0x4000_0000, 0xC000_0000, 1, 2, 3]);
}

#[test]
fn a_float_converted_to_an_integer_saturates_to_the_nearest_value_its_float_holds() {
    let floats = [
        1e10f32,
        -1e10,
        f32::INFINITY,
        f32::NEG_INFINITY,
        3.7,
        -3.7,
        4_294_967_040.0,
        f32::NAN,
    ];
    let input: Vec<u32> = floats.iter().map(|&x| bits(x)).collect();
    let out = run(
        "
        @compute @workgroup_size(1)
        fn main() {
            for (var i = 0u; i < 8u; i++) {
                let x = bitcast<f32>(input[i]);
                out[i * 2u] = bitcast<u32>(i32(x));
                out[i * 2u + 1u] = u32(x);
            }
        }
        ",
        &input,
        16,
    );
    let expected: [(i32, u32); 8] = [
        (2_147_483_520, 4_294_967_040),
        (-2_147_483_648, 0),
        (2_147_483_520, 4_294_967_040),
        (-2_147_483_648, 0),
        (3, 3),
        (-3, 0),
        (2_147_483_520, 4_294_967_040),
        (0, 0),
    ];
    for (at, (signed, unsigned)) in expected.iter().enumerate() {
        assert_eq!(out[at * 2], signed.cast_unsigned(), "i32 of {}", floats[at]);
        assert_eq!(out[at * 2 + 1], *unsigned, "u32 of {}", floats[at]);
    }
}

#[test]
fn the_bit_functions_are_wgsls_at_zero_all_ones_and_past_the_width() {
    let out = run(
        "
        @compute @workgroup_size(1)
        fn main() {
            let zero = input[0];
            let ones = input[1];
            let word = input[2];
            out[0] = countLeadingZeros(zero);
            out[1] = countLeadingZeros(word);
            out[2] = countTrailingZeros(zero);
            out[3] = countTrailingZeros(word);
            out[4] = firstLeadingBit(zero);
            out[5] = firstLeadingBit(word);
            out[6] = bitcast<u32>(firstLeadingBit(bitcast<i32>(ones)));
            out[7] = bitcast<u32>(firstLeadingBit(bitcast<i32>(0xFFFF0000u)));
            out[8] = firstTrailingBit(zero);
            out[9] = firstTrailingBit(word);
            out[10] = countOneBits(word);
            out[11] = reverseBits(word);
            out[12] = extractBits(word, 4u, 8u);
            out[13] = bitcast<u32>(extractBits(bitcast<i32>(word), 28u, 4u));
            out[14] = extractBits(word, input[3], input[4]);
            out[15] = extractBits(word, input[5], input[4]);
            out[16] = bitcast<u32>(extractBits(bitcast<i32>(word), 0u, 0u));
            out[17] = insertBits(zero, ones, 8u, 4u);
            out[18] = insertBits(word, zero, input[9], input[6]);
            out[19] = insertBits(word, ones, input[7], input[8]);
            out[20] = bitcast<u32>(countLeadingZeros(bitcast<i32>(word)));
        }
        ",
        &[0, u32::MAX, 0x8000_1F00, 40, 8, 30, 100, 32, 1, 28],
        21,
    );
    let word = 0x8000_1F00u32;
    assert_eq!(
        out,
        [
            32,
            0,
            32,
            8,
            u32::MAX,
            31,
            u32::MAX,
            15,
            u32::MAX,
            8,
            word.count_ones(),
            word.reverse_bits(),
            (word >> 4) & 0xFF,
            0xFFFF_FFF8,
            0,
            2,
            0,
            0x0000_0F00,
            0x0000_1F00,
            word,
            0,
        ]
    );
}

#[test]
fn the_packings_round_half_up_and_saturate_as_wgsl_writes_them() {
    let x = [0.25f32, -0.25, 2.0, -2.0];
    let input: Vec<u32> = x
        .iter()
        .map(|&v| bits(v))
        .chain([0x7F80_01FF, 0xFFFF_FF80])
        .collect();
    let out = run(
        "
        @compute @workgroup_size(1)
        fn main() {
            let v = vec4f(bitcast<f32>(input[0]), bitcast<f32>(input[1]), bitcast<f32>(input[2]), bitcast<f32>(input[3]));
            out[0] = pack4x8snorm(v);
            out[1] = pack4x8unorm(v);
            out[2] = pack2x16snorm(v.zw);
            out[3] = pack2x16unorm(v.xz);
            out[4] = pack4xI8(vec4i(-1, 127, 128, -129));
            out[5] = pack4xU8(vec4u(1u, 255u, 256u, 511u));
            out[6] = pack4xI8Clamp(vec4i(-1, 300, -300, 5));
            out[7] = pack4xU8Clamp(vec4u(1u, 300u, 255u, 0u));
            let signed = unpack4xI8(input[4]);
            out[8] = bitcast<u32>(signed.x);
            out[9] = bitcast<u32>(signed.y);
            out[10] = bitcast<u32>(signed.z);
            out[11] = bitcast<u32>(signed.w);
            let unsigned = unpack4xU8(input[4]);
            out[12] = unsigned.x + (unsigned.y << 8u) + (unsigned.z << 16u) + (unsigned.w << 24u);
            out[13] = dot4U8Packed(input[4], input[5]);
            out[14] = bitcast<u32>(dot4I8Packed(input[4], input[5]));
        }
        ",
        &input,
        15,
    );
    // floor(0.5 + 127 × 0.25) = 32 and floor(0.5 − 31.75) = −32: rounded
    // down from the half above, so a negative value rounds away from zero.
    assert_eq!(out[0], 0x81_7F_E0_20);
    assert_eq!(out[1], 0x00_FF_00_40);
    assert_eq!(out[2], 0x8001_7FFF);
    assert_eq!(out[3], 0xFFFF_4000);
    assert_eq!(out[4], 0x7F_80_7F_FF);
    assert_eq!(out[5], 0xFF_00_FF_01);
    assert_eq!(out[6], 0x05_80_7F_FF);
    assert_eq!(out[7], 0x00_FF_FF_01);
    assert_eq!(
        out[8..12],
        [(-1i32).cast_unsigned(), 1, (-128i32).cast_unsigned(), 127]
    );
    assert_eq!(out[12], 0x7F80_01FF);
    let bytes = |w: u32| w.to_le_bytes();
    let unsigned_dot: u32 = bytes(0x7F80_01FF)
        .iter()
        .zip(bytes(0xFFFF_FF80))
        .map(|(&a, b)| u32::from(a) * u32::from(b))
        .sum();
    let signed_dot: i32 = bytes(0x7F80_01FF)
        .iter()
        .zip(bytes(0xFFFF_FF80))
        .map(|(&a, b)| i32::from(a.cast_signed()) * i32::from(b.cast_signed()))
        .sum();
    assert_eq!(out[13], unsigned_dot);
    assert_eq!(out[14], signed_dot.cast_unsigned());
}

#[test]
fn minima_maxima_and_clamps_are_wgsls_with_nan_and_crossed_bounds() {
    let input = [
        bits(f32::NAN),
        bits(1.0),
        bits(5.0),
        bits(2.0),
        0x8000_0000,
        7,
        9,
        3,
    ];
    let out = run(
        "
        @compute @workgroup_size(1)
        fn main() {
            let nan = bitcast<f32>(input[0]);
            let one = bitcast<f32>(input[1]);
            let five = bitcast<f32>(input[2]);
            let two = bitcast<f32>(input[3]);
            out[0] = bitcast<u32>(min(nan, one));
            out[1] = bitcast<u32>(max(one, nan));
            out[2] = bitcast<u32>(clamp(two, five, one));
            out[3] = bitcast<u32>(clamp(two, one, five));
            out[4] = bitcast<u32>(saturate(five));
            out[5] = bitcast<u32>(saturate(-five));
            out[6] = bitcast<u32>(abs(bitcast<i32>(input[4])));
            out[7] = bitcast<u32>(clamp(i32(input[5]), i32(input[6]), i32(input[7])));
            out[8] = clamp(input[5], input[6], input[7]);
            out[9] = bitcast<u32>(smoothstep(five, one, two));
            out[10] = bitcast<u32>(smoothstep(one, five, two));
            out[11] = bitcast<u32>(sign(-two) + sign(0.0 * two) + 4.0 * sign(two));
            out[12] = bitcast<u32>(sign(-7) + sign(0) * 2 + 4 * sign(9));
        }
        ",
        &input,
        13,
    );
    assert_eq!(out[0], bits(1.0), "the operand that is not NaN");
    assert_eq!(out[1], bits(1.0), "the operand that is not NaN");
    assert_eq!(
        out[2],
        bits(1.0),
        "min(max(2, 5), 1) where the bounds cross"
    );
    assert_eq!(out[3], bits(2.0));
    assert_eq!(out[4], bits(1.0));
    assert_eq!(out[5], bits(0.0));
    assert_eq!(
        out[6], 0x8000_0000,
        "the most negative i32 is its own magnitude"
    );
    assert_eq!(out[7], 3, "min(max(7, 9), 3)");
    assert_eq!(out[8], 3);
    // t = clamp((2 - 5) / (1 - 5)) = 0.75: 0.75² × (3 − 1.5).
    assert_eq!(out[9], bits(0.84375), "smoothstep with its edges reversed");
    assert_eq!(out[10], bits(0.15625));
    assert_eq!(out[11], bits(3.0));
    assert_eq!(out[12], 3);
}

#[test]
fn a_short_circuit_runs_its_right_side_only_where_the_left_does_not_decide() {
    let out = run(
        "
        var<private> calls: u32;
        fn counted(result: bool) -> bool {
            calls += 1u;
            return result;
        }
        @compute @workgroup_size(1)
        fn main() {
            let yes = input[0] == 1u;
            let no = !yes;
            let a = no && counted(true);
            let b = yes || counted(false);
            let c = yes && counted(true);
            let d = no || (counted(false) || counted(true));
            out[0] = calls;
            out[1] = u32(a) + 2u * u32(b) + 4u * u32(c) + 8u * u32(d);
        }
        ",
        &[1],
        2,
    );
    assert_eq!(out, [3, 2 | 4 | 8]);
}

#[test]
fn control_flow_reaches_the_values_wgsl_gives_it() {
    let out = run(
        "
        fn classify(x: i32) -> u32 {
            if x < 0 { return 1u; } else if x == 0 { return 2u; } else if x < 10 { return 3u; }
            return 4u;
        }
        @compute @workgroup_size(1)
        fn main() {
            var total = 0u;
            for (var i = 0u; i < 10u; i++) {
                if i == 3u { continue; }
                if i == 8u { break; }
                total += i;
            }
            out[0] = total;
            var j = 0u;
            while j < 5u { j += 2u; }
            out[1] = j;
            var k = 10u;
            var steps = 0u;
            loop {
                k -= 1u;
                if k == 7u { continue; }
                steps += 1u;
                continuing { break if k <= 4u; }
            }
            out[2] = steps;
            out[3] = k;
            for (var c = 0u; c < 4u; c++) {
                switch c {
                    case 0u, 1u: { out[4 + c] = 10u; }
                    case 2u: { out[4 + c] = 20u; break; }
                    default: { out[4 + c] = 30u; }
                }
            }
            out[8] = classify(-5) * 1000u + classify(0) * 100u + classify(7) * 10u + classify(70);
        }
        ",
        &[],
        9,
    );
    assert_eq!(out, [1 + 2 + 4 + 5 + 6 + 7, 6, 5, 4, 10, 10, 20, 30, 1234]);
}

#[test]
fn a_switch_within_a_clause_selects_on_its_own_cases() {
    let out = run(
        "
        @compute @workgroup_size(1)
        fn main() {
            for (var c = 0u; c < 3u; c++) {
                for (var d = 0u; d < 3u; d++) {
                    var r = 0u;
                    switch c {
                        case 1u: {
                            switch d {
                                case 1u: { r = 11u; }
                                default: { r = 10u; }
                            }
                        }
                        default: {
                            switch d {
                                case 0u, 2u: { r = 20u + d; }
                                default: { r = 29u; break; }
                            }
                            r += 100u;
                        }
                    }
                    out[c * 3u + d] = r;
                }
            }
        }
        ",
        &[],
        9,
    );
    assert_eq!(out, [120, 129, 122, 10, 11, 10, 120, 129, 122]);
}

#[test]
fn pointers_write_through_their_roots_and_element_indexes() {
    let out = run(
        "
        struct Item { weight: u32, tags: array<u32, 4> }
        var<private> items: array<Item, 4>;
        fn bump(p: ptr<function, u32>) { *p += 1u; }
        fn tag(t: ptr<private, u32>, by: u32) { *t = by * 3u; }
        fn weigh(item: ptr<private, Item>) { (*item).weight = 9u; item.tags[2] = 7u; }
        fn into(slot: ptr<storage, u32, read_write>, value: u32) { *slot = value; }
        @compute @workgroup_size(1)
        fn main() {
            var n = 0u;
            bump(&n);
            let p = &n;
            bump(p);
            out[0] = n;
            for (var i = 0u; i < 4u; i++) {
                tag(&items[i].tags[i % 4u], i + 1u);
            }
            weigh(&items[2]);
            out[1] = items[0].tags[0];
            out[2] = items[1].tags[1];
            out[3] = items[2].weight;
            out[4] = items[2].tags[2];
            out[5] = items[3].tags[3];
            into(&out[6], 66u);
            into(&out[input[0]], 77u);
        }
        ",
        &[7],
        8,
    );
    assert_eq!(out, [2, 3, 6, 9, 7, 12, 66, 77]);
}

#[test]
fn values_are_indexed_at_run_time_through_memory_and_by_lane() {
    let out = run(
        "
        const table = array(1u, 2u, 4u, 8u);
        @compute @workgroup_size(1)
        fn main() {
            for (var i = 0u; i < 4u; i++) {
                let m = mat2x2f(1.0, 2.0, 3.0, 4.0);
                let v = vec4u(9u, 8u, 7u, 6u);
                let local = array(m, m * 2.0);
                out[i] = table[i] + v[i] * 10u + u32(m[i % 2u][(i + 1u) % 2u]) * 100u + u32(local[i / 2u][i % 2u].y) * 1000u;
            }
        }
        ",
        &[],
        4,
    );
    assert_eq!(
        out,
        [
            1 + 90 + 200 + 2000,
            2 + 80 + 300 + 4000,
            4 + 70 + 200 + 4000,
            8 + 60 + 300 + 8000
        ]
    );
}

#[test]
fn matrices_multiply_as_linear_algebra() {
    let out = run(
        "
        @compute @workgroup_size(1)
        fn main() {
            let m = mat2x2f(1.0, 2.0, 3.0, 4.0);
            let v = vec2f(5.0, 6.0);
            let mv = m * v;
            let vm = v * m;
            let mm = m * m;
            let t = transpose(m);
            out[0] = u32(mv.x);
            out[1] = u32(mv.y);
            out[2] = u32(vm.x);
            out[3] = u32(vm.y);
            out[4] = u32(mm[0].x) * 1000u + u32(mm[0].y) * 100u + u32(mm[1].x) * 10u + u32(mm[1].y);
            out[5] = u32(t[0].y);
            out[6] = bitcast<u32>(determinant(m));
            out[7] = u32((m + m)[1].y) + u32((m * 3.0)[0].x) * 100u;
        }
        ",
        &[],
        8,
    );
    assert_eq!(out[..4], [23, 34, 17, 39]);
    // [[1, 3], [2, 4]]² = [[7, 15], [10, 22]], read column by column.
    assert_eq!(out[4], 7 * 1000 + 10 * 100 + 15 * 10 + 22);
    assert_eq!(out[5], 3);
    assert_eq!(out[6], bits(-2.0));
    assert_eq!(out[7], 8 + 300);
}

#[test]
fn assignments_write_lanes_members_and_compound_results() {
    let out = run(
        "
        requires swizzle_assignment;
        struct Body { position: vec4u, angles: array<vec3u, 2> }
        @compute @workgroup_size(1)
        fn main() {
            var b = Body(vec4u(1u, 2u, 3u, 4u), array(vec3u(5u), vec3u(6u)));
            b.position.xz = vec2u(10u, 30u);
            b.position.yw += vec2u(100u);
            b.angles[1].y -= 1u;
            b.angles[0] *= 2u;
            var counter = 5u;
            counter++;
            counter--;
            counter <<= 2u;
            counter |= 1u;
            out[0] = b.position.x;
            out[1] = b.position.y;
            out[2] = b.position.z;
            out[3] = b.position.w;
            out[4] = b.angles[1].y;
            out[5] = b.angles[0].z;
            out[6] = counter;
        }
        ",
        &[],
        7,
    );
    assert_eq!(out, [10, 102, 30, 104, 5, 10, 21]);
}

#[test]
fn atomics_return_what_they_found_and_compare_exchange_reports_whether_it_did() {
    let out = run(
        "
        var<workgroup> counter: atomic<u32>;
        var<workgroup> signed: atomic<i32>;
        @compute @workgroup_size(4)
        fn main(@builtin(local_invocation_index) lane: u32) {
            atomicAdd(&counter, lane + 1u);
            atomicMin(&signed, -i32(lane));
            workgroupBarrier();
            if lane == 0u {
                out[0] = atomicLoad(&counter);
                out[1] = bitcast<u32>(atomicLoad(&signed));
                out[2] = atomicExchange(&counter, 50u);
                let failed = atomicCompareExchangeWeak(&counter, 1u, 2u);
                out[3] = failed.old_value;
                out[4] = u32(failed.exchanged);
                let swapped = atomicCompareExchangeWeak(&counter, 50u, 60u);
                out[5] = swapped.old_value + 1000u * u32(swapped.exchanged);
                atomicStore(&counter, 0xF0u);
                out[6] = atomicOr(&counter, 0x0Fu);
                out[7] = atomicAnd(&counter, 0x3Cu);
                out[8] = atomicXor(&counter, 0xFFu);
                out[9] = atomicSub(&counter, 1u);
                out[10] = atomicMax(&counter, 0x1000u);
                out[11] = atomicLoad(&counter);
            }
        }
        ",
        &[],
        12,
    );
    assert_eq!(
        out,
        [
            10,
            (-3i32).cast_unsigned(),
            10,
            50,
            0,
            1050,
            0xF0,
            0xFF,
            0x3C,
            0xC3,
            0xC2,
            0x1000
        ]
    );
}

#[test]
fn workgroup_memory_starts_zeroed_and_is_shared_across_a_barrier() {
    let out = run_with(
        "
        var<workgroup> tile: array<u32, 300>;
        var<workgroup> flag: u32;
        @compute @workgroup_size(8, 2)
        fn main(@builtin(local_invocation_index) lane: u32,
                @builtin(workgroup_index) group: u32,
                @builtin(global_invocation_index) global: u32) {
            let before = tile[lane * 17u] + flag;
            workgroupBarrier();
            tile[lane] = lane + 1u;
            workgroupBarrier();
            let read_once = workgroupUniformLoad(&flag);
            out[global] = before * 1000u + tile[(lane + 1u) % 16u] + group * 100u + read_once;
        }
        ",
        &Options::default(),
        &[],
        3,
        48,
    );
    // Three workgroups of 8 × 2 laid row by row: a row of the dispatch is
    // 24 invocations wide.
    for (global, &word) in out.iter().enumerate() {
        let global = u32::try_from(global).expect("small");
        let (x, y) = (global % 24, global / 24);
        let (group, lane) = (x / 8, y * 8 + x % 8);
        assert_eq!(
            word,
            (lane + 1) % 16 + 1 + group * 100,
            "invocation {global}"
        );
    }
}

#[test]
fn overrides_take_the_values_the_compile_gives_them() {
    let body = "
        override width: u32 = 4;
        @id(7) override scale: u32;
        override doubled = width * 2u;
        var<workgroup> row: array<u32, doubled>;
        @compute @workgroup_size(width)
        fn main(@builtin(local_invocation_index) lane: u32) {
            row[lane] = lane * scale;
            row[lane + width] = 100u;
            workgroupBarrier();
            out[lane] = row[(lane + 1u) % doubled] + doubled * 1000u;
        }
    ";
    let options = Options {
        overrides: &[(OverrideKey::Id(7), 3.0), (OverrideKey::Name("width"), 2.0)],
    };
    let out = run_with(body, &options, &[], 1, 2);
    assert_eq!(out, [3 + 4000, 100 + 4000]);
}

#[test]
fn structures_cross_between_buffers_private_and_function_memory() {
    let out = run(
        "
        struct Pair { a: u32, b: vec3u }
        struct Holder { pairs: array<Pair, 3>, count: u32 }
        var<private> kept: Holder;
        @compute @workgroup_size(1)
        fn main() {
            var local: Holder;
            for (var i = 0u; i < 3u; i++) {
                local.pairs[i] = Pair(input[i], vec3u(input[i] + 1u));
            }
            local.count = arrayLength(&input);
            kept = local;
            let copy = kept;
            out[0] = copy.pairs[2].a;
            out[1] = copy.pairs[1].b.z;
            out[2] = copy.count;
        }
        ",
        &[11, 22, 33],
        3,
    );
    assert_eq!(out, [33, 23, u32::try_from(WORDS).expect("small")]);
}

#[test]
fn select_takes_a_scalar_condition_for_a_vector() {
    let out = run(
        "
        @compute @workgroup_size(1)
        fn main() {
            let yes = input[0] == 1u;
            let v = select(vec2u(1u, 2u), vec2u(3u, 4u), yes);
            let w = select(vec2u(1u, 2u), vec2u(3u, 4u), vec2(yes, !yes));
            out[0] = v.x;
            out[1] = v.y;
            out[2] = w.x;
            out[3] = w.y;
            out[4] = u32(all(vec2(yes, yes))) + 2u * u32(any(vec2(!yes, !yes)));
        }
        ",
        &[1],
        5,
    );
    assert_eq!(out, [3, 4, 3, 2, 1]);
}

#[test]
fn half_floats_are_computed_and_converted() {
    let out = run(
        "
        enable f16;
        @compute @workgroup_size(1)
        fn main() {
            let h = f16(input[0]) * 1.5h + 0.25h;
            out[0] = bitcast<u32>(vec2h(h, -h));
            out[1] = u32(h * 4.0h);
            out[2] = bitcast<u32>(f32(h));
            out[3] = bitcast<u32>(i32(-h));
        }
        ",
        &[3, 0],
        4,
    );
    // 3 × 1.5 + 0.25 = 4.75: 0x44C0 in binary16.
    assert_eq!(out[0], 0xC4C0_44C0);
    assert_eq!(out[1], 19);
    assert_eq!(out[2], bits(4.75));
    assert_eq!(out[3], (-4i32).cast_unsigned());
}

#[test]
fn a_compile_is_deterministic() {
    let source = format!(
        "{BINDINGS}
        @compute @workgroup_size(8)
        fn main(@builtin(global_invocation_id) id: vec3u) {{
            out[id.x] = input[id.x] * 3u + countOneBits(id.x);
        }}"
    );
    let first = compile(&source, &Options::default()).expect("compiled");
    for _ in 0..4 {
        assert_eq!(
            compile(&source, &Options::default())
                .expect("compiled")
                .module,
            first.module
        );
    }
}

/// The side of the square target a render test draws into.
const SIDE: u32 = 8;

/// The texels of the picture a render test may sample, row by row: red and
/// green, then blue and white.
const PICTURE: [[u8; 4]; 4] = [
    [255, 0, 0, 255],
    [0, 255, 0, 255],
    [0, 0, 255, 255],
    [255, 255, 255, 255],
];

/// What a render test drew: the target's texels, row by row, and the first
/// words its fragments wrote to binding 0.
struct Drawn {
    texels: Vec<[u8; 4]>,
    words: Vec<u32>,
}

/// The layout and set every render test binds: the two buffers, then
/// [`PICTURE`] and a nearest sampler, which a shader declares where it reads
/// them.
fn render_bindings(
    context: &Context,
    (output, input): (&Buffer, &Buffer),
    (picture, nearest): (&tairix_gpu::ImageView, &tairix_gpu::Sampler),
) -> (BindSetLayout, BindSet) {
    let stages = Stages::VERTEX.with(Stages::FRAGMENT);
    let layout = context
        .create_bind_set_layout(&[
            LayoutBinding {
                binding: 0,
                kind: DescriptorType::StorageBuffer,
                count: 1,
                access: Access::READ.with(Access::WRITE),
                stages,
            },
            LayoutBinding {
                binding: 1,
                kind: DescriptorType::StorageBuffer,
                count: 1,
                access: Access::READ,
                stages,
            },
            LayoutBinding {
                binding: 2,
                kind: DescriptorType::SampledImage(
                    ImageShape {
                        dim: Dim::DIM_2D,
                        arrayed: false,
                        multisampled: false,
                        component: Component::Float,
                        format: ImageFormat::UNKNOWN,
                    },
                    SampleType::Float,
                ),
                count: 1,
                access: Access::READ,
                stages,
            },
            LayoutBinding {
                binding: 3,
                kind: DescriptorType::Sampler(SamplerType::Filtering),
                count: 1,
                access: Access::READ,
                stages,
            },
        ])
        .expect("a set layout");
    let whole = |buffer| {
        [Resource::Buffer {
            buffer,
            offset: 0,
            size: WORDS * 4,
        }]
    };
    let (written, read) = (whole(output), whole(input));
    let set = context
        .create_bind_set(
            &layout,
            &[
                BindSetEntry {
                    binding: 0,
                    resources: &written,
                },
                BindSetEntry {
                    binding: 1,
                    resources: &read,
                },
                BindSetEntry {
                    binding: 2,
                    resources: &[Resource::Image(picture)],
                },
                BindSetEntry {
                    binding: 3,
                    resources: &[Resource::Sampler(nearest)],
                },
            ],
        )
        .expect("a set");
    (layout, set)
}

/// A two-dimensional image of `extent` and its one view.
fn image(
    context: &Context,
    extent: (u32, u32),
    usage: ImageUsage,
) -> (
    tairix_gpu::Allocation,
    tairix_gpu::Image,
    tairix_gpu::ImageView,
) {
    let desc = ImageDesc {
        dimension: ImageDimension::D2,
        format: Format::Rgba8Unorm,
        extent: Extent3d {
            width: extent.0,
            height: extent.1,
            depth: 1,
        },
        mip_levels: 1,
        array_layers: 1,
        samples: 1,
        usage,
    };
    let needs = context.image_requirements(&desc).expect("requirements");
    let memory = context
        .allocate(&AllocationDesc {
            size: needs.size,
            memory_type: 0,
        })
        .expect("memory");
    let image = context.create_image(&memory, 0, &desc).expect("an image");
    let view = context
        .create_image_view(
            &image,
            &ImageViewDesc {
                format: Format::Rgba8Unorm,
                dimension: ViewDimension::D2,
                range: ImageRange {
                    aspects: Format::Rgba8Unorm.aspects(),
                    base_level: 0,
                    levels: 1,
                    base_layer: 0,
                    layers: 1,
                },
            },
        )
        .expect("a view");
    (memory, image, view)
}

/// The copy of all of a two-dimensional `extent` to or from a buffer.
fn whole_image(extent: (u32, u32)) -> BufferImageCopy {
    BufferImageCopy {
        buffer_offset: 0,
        bytes_per_row: extent.0 * 4,
        rows_per_image: extent.1,
        image: ImageSubresource {
            aspect: Format::Rgba8Unorm.aspects(),
            level: 0,
            base_layer: 0,
            layers: 1,
        },
        origin: Origin3d::default(),
        extent: Extent3d {
            width: extent.0,
            height: extent.1,
            depth: 1,
        },
    }
}

/// [`PICTURE`] on the device, as an image and its view.
fn picture(
    gpu: &Gpu,
) -> (
    tairix_gpu::Allocation,
    tairix_gpu::Image,
    tairix_gpu::ImageView,
) {
    let made = image(
        &gpu.context,
        (2, 2),
        ImageUsage::SAMPLED.with(ImageUsage::TRANSFER_DST),
    );
    let (staging_memory, staging) = gpu.buffer(16, BufferUsage::TRANSFER_SRC);
    staging_memory
        .write(0, PICTURE.as_flattened())
        .expect("written");
    let mut upload = gpu.context.command_buffer();
    upload
        .copy_buffer_to_image(&staging, &made.1, &[whole_image((2, 2))])
        .expect("recorded");
    gpu.submit(&upload);
    made
}

/// The pipeline of `shader`'s `vs` and `fs` under `layout`, drawing
/// triangles into one `Rgba8Unorm` target.
fn render_pipeline(
    context: &Context,
    layout: &tairix_gpu::PipelineLayout,
    shader: &tairix_gpu::ShaderModule,
) -> tairix_gpu::RenderPipeline {
    let stage = |entry_point| StageDesc {
        module: shader,
        entry_point,
        specialization: &[],
    };
    context
        .create_render_pipeline(&RenderPipelineDesc {
            layout,
            vertex: stage("vs"),
            fragment: Some(stage("fs")),
            vertex_buffers: &[],
            topology: Topology::Triangles,
            primitive_restart: false,
            rasterization: Rasterization {
                cull: CullMode::None,
                front_face: FrontFace::CounterClockwise,
                depth_bias: None,
                depth_clamp: false,
                line_width: 1.0,
            },
            depth_stencil: None,
            multisample: Multisample {
                samples: 1,
                mask: u32::MAX,
                alpha_to_coverage: false,
            },
            targets: &[ColourTarget {
                format: Format::Rgba8Unorm,
                blend: None,
                write_mask: ColourMask::ALL,
            }],
            logic_op: None,
        })
        .unwrap_or_else(|error| panic!("no pipeline: {error:?}"))
}

/// Draw one triangle over a [`SIDE`]-square target, cleared to zero, with
/// `body`'s `vs` and `fs`, binding 1 holding `input`.
fn draw(body: &str, input: &[u32], count: usize) -> Drawn {
    let source = with_bindings(body);
    let gpu = Gpu::new();
    let context = &gpu.context;
    let shader = context
        .create_shader_module(admitted(&source, &Options::default()))
        .expect("a module");
    let (output_memory, output) = gpu.buffer(WORDS * 4, BufferUsage::STORAGE);
    let (input_memory, input_buffer) = gpu.buffer(WORDS * 4, BufferUsage::STORAGE);
    input_memory
        .write(
            0,
            &bytes_of(input, usize::try_from(WORDS * 4).expect("small")),
        )
        .expect("written");
    let (_picture_memory, _picture, picture_view) = picture(&gpu);
    let nearest = context
        .create_sampler(&SamplerDesc {
            mag_filter: Filter::Nearest,
            min_filter: Filter::Nearest,
            mipmap_filter: Filter::Nearest,
            ..SamplerDesc::LINEAR
        })
        .expect("a sampler");
    let (set_layout, set) =
        render_bindings(context, (&output, &input_buffer), (&picture_view, &nearest));
    let layout = context
        .create_pipeline_layout(&[&set_layout], PushConstants::NONE)
        .expect("a pipeline layout");
    let pipeline = render_pipeline(context, &layout, &shader);
    let (_target_memory, target, target_view) = image(
        context,
        (SIDE, SIDE),
        ImageUsage::COLOR_ATTACHMENT.with(ImageUsage::TRANSFER_SRC),
    );
    let area = Rect {
        x: 0,
        y: 0,
        width: SIDE,
        height: SIDE,
    };
    let viewport = Viewport {
        x: 0.0,
        y: 0.0,
        width: 8.0,
        height: 8.0,
        min_depth: 0.0,
        max_depth: 1.0,
    };
    let mut commands = context.command_buffer();
    commands
        .render_pass(
            &Pass {
                colour: &[ColourAttachment {
                    view: &target_view,
                    resolve: None,
                    load: Load::Clear(ClearValue::Float([0.0; 4])),
                }],
                depth_stencil: None,
                area,
            },
            |drawer| {
                drawer.draw(
                    &Draw {
                        pipeline: &pipeline,
                        sets: &[&set],
                        push_constants: &[],
                        vertex_buffers: &[],
                        viewport,
                        scissor: area,
                        stencil_reference: 0,
                        blend_constant: [0.0; 4],
                    },
                    0..3,
                    0..1,
                )
            },
        )
        .expect("recorded");
    gpu.submit(&commands);
    let (readback_memory, readback) =
        gpu.buffer(u64::from(SIDE * SIDE * 4), BufferUsage::TRANSFER_DST);
    let mut copy = context.command_buffer();
    copy.copy_image_to_buffer(&target, &readback, &[whole_image((SIDE, SIDE))])
        .expect("recorded");
    gpu.submit(&copy);
    let mut texels = vec![0u8; usize::try_from(SIDE * SIDE * 4).expect("small")];
    readback_memory.read(0, &mut texels).expect("read back");
    let mut written = vec![0u8; count * 4];
    output_memory.read(0, &mut written).expect("read back");
    Drawn {
        texels: texels.as_chunks::<4>().0.to_vec(),
        words: words_of(&written),
    }
}

/// A vertex shader drawing one triangle over the whole target, `corner`
/// running from 0 to 1 across it.
const COVERING: &str = "
    struct Varyings {
        @builtin(position) at: vec4f,
        @location(0) corner: vec2f,
        @location(1) @interpolate(flat) id: u32,
    }
    @vertex
    fn vs(@builtin(vertex_index) v: u32) -> Varyings {
        let corner = vec2f(f32((v << 1u) & 2u), f32(v & 2u));
        return Varyings(vec4f(corner * 2.0 - 1.0, 0.0, 1.0), corner, 7u + v - v);
    }
";

#[test]
fn a_discarded_fragment_writes_no_memory_and_no_colour() {
    let drawn = draw(
        &format!(
            "{COVERING}
            @fragment
            fn fs(in: Varyings) -> @location(0) vec4f {{
                let x = u32(in.at.x);
                let index = u32(in.at.y) * 8u + x;
                if x < 4u {{ discard; }}
                out[index] = index + 1u;
                return vec4f(1.0, 0.0, 0.0, 1.0);
            }}"
        ),
        &[],
        64,
    );
    for (index, (&word, texel)) in drawn.words.iter().zip(&drawn.texels).enumerate() {
        let kept = index % 8 >= 4;
        let index = u32::try_from(index).expect("small");
        assert_eq!(
            word,
            if kept { index + 1 } else { 0 },
            "the store of fragment {index}"
        );
        assert_eq!(
            *texel,
            if kept { [255, 0, 0, 255] } else { [0; 4] },
            "the colour of fragment {index}"
        );
    }
}

#[test]
fn derivatives_and_the_stage_interface_reach_the_fragment() {
    let drawn = draw(
        &format!(
            "{COVERING}
            @fragment
            fn fs(in: Varyings) -> @location(0) vec4f {{
                let field = in.at.x * in.at.y + in.at.x;
                let index = (u32(in.at.y) * 8u + u32(in.at.x)) * 4u;
                out[index] = bitcast<u32>(dpdxFine(field));
                out[index + 1u] = bitcast<u32>(dpdyCoarse(field));
                out[index + 2u] = bitcast<u32>(fwidth(in.at.x));
                out[index + 3u] = in.id;
                return vec4f(in.corner, 0.0, 1.0);
            }}"
        ),
        &[],
        256,
    );
    for y in 0..SIDE {
        for x in 0..SIDE {
            let at = usize::try_from((y * SIDE + x) * 4).expect("small");
            let row = f32::from(u16::try_from(y).expect("small")) + 0.5;
            let left = f32::from(u16::try_from(x & !1).expect("small")) + 0.5;
            assert_eq!(
                drawn.words[at],
                (row + 1.0).to_bits(),
                "dpdxFine at ({x}, {y})"
            );
            assert_eq!(
                drawn.words[at + 1],
                left.to_bits(),
                "dpdyCoarse at ({x}, {y})"
            );
            assert_eq!(
                drawn.words[at + 2],
                1.0f32.to_bits(),
                "fwidth at ({x}, {y})"
            );
            assert_eq!(drawn.words[at + 3], 7, "a flat value at ({x}, {y})");
            let texel = drawn.texels[usize::try_from(y * SIDE + x).expect("small")];
            let expected = |n: u32| {
                (255.0 * (f32::from(u16::try_from(n).expect("small")) + 0.5) / 8.0).round()
            };
            for (channel, n) in [(0, x), (1, y)] {
                let difference = (f32::from(texel[channel]) - expected(n)).abs();
                assert!(
                    difference <= 1.0,
                    "channel {channel} at ({x}, {y}): {texel:?}"
                );
            }
        }
    }
}

#[test]
fn a_picture_is_sampled_loaded_and_queried() {
    let drawn = draw(
        &format!(
            "{COVERING}
            @group(0) @binding(2) var picture: texture_2d<f32>;
            @group(0) @binding(3) var nearest: sampler;
            @fragment
            fn fs(in: Varyings) -> @location(0) vec4f {{
                let x = u32(in.at.x);
                let y = u32(in.at.y);
                let index = (y * 8u + x) * 4u;
                let uv = in.at.xy / 8.0;
                let sampled = textureSample(picture, nearest, uv);
                out[index] = pack4x8unorm(sampled);
                out[index + 1u] = pack4x8unorm(textureLoad(picture, vec2u(x / 4u, y / 4u), 0));
                out[index + 2u] = textureDimensions(picture).x * 10u + textureNumLevels(picture);
                out[index + 3u] = pack4x8unorm(textureSampleLevel(picture, nearest, uv, 0.0));
                return sampled;
            }}"
        ),
        &[],
        256,
    );
    for y in 0..SIDE {
        for x in 0..SIDE {
            let pixel = usize::try_from(y * SIDE + x).expect("small");
            let texel = PICTURE[usize::try_from((y / 4) * 2 + x / 4).expect("small")];
            let packed = u32::from_le_bytes(texel);
            let at = pixel * 4;
            assert_eq!(drawn.words[at], packed, "sampled at ({x}, {y})");
            assert_eq!(drawn.words[at + 1], packed, "loaded at ({x}, {y})");
            assert_eq!(drawn.words[at + 2], 21, "two texels wide, one level");
            assert_eq!(
                drawn.words[at + 3],
                packed,
                "sampled at its level at ({x}, {y})"
            );
            assert_eq!(drawn.texels[pixel], texel, "drawn at ({x}, {y})");
        }
    }
}

#[test]
fn an_index_past_its_level_reads_zero_and_writes_nothing_outside_a_buffer() {
    let source = with_bindings(
        "
        var<private> grid: array<array<u32, 4>, 4>;
        var<workgroup> shared_grid: array<array<u32, 4>, 2>;
        @compute @workgroup_size(1)
        fn main() {
            let past = input[0];
            grid[1][1] = 11u;
            grid[0][past] = 99u;
            out[0] = grid[1][1];
            out[1] = grid[0][past];
            var local = array(vec4u(1u, 2u, 3u, 4u), vec4u(5u, 6u, 7u, 8u));
            local[0][past] = 50u;
            out[2] = local[1][1];
            out[3] = local[0][past];
            shared_grid[1][1] = 21u;
            shared_grid[0][past] = 98u;
            out[4] = shared_grid[1][1];
            out[5] = shared_grid[past][0];
        }
        ",
    );
    let gpu = Gpu::new();
    let module = || admitted(&source, &Options::default());
    let expected = [11, 0, 6, 0, 21, 0];
    assert_eq!(gpu.compute(module(), &[5], 1, 6), expected);
    assert_eq!(
        gpu.compute(guarded(module()), &[5], 1, 6),
        expected,
        "guarded, as a device bounding only buffers runs it"
    );
}

#[test]
fn an_index_past_its_level_in_a_buffer_reaches_the_byte_its_layout_places() {
    let source = "
        struct Wide { head: array<u32, 4>, rest: array<u32> }
        @group(0) @binding(0) var<storage, read_write> out: Wide;
        @group(0) @binding(1) var<storage, read> input: array<u32>;
        @compute @workgroup_size(1)
        fn main() {
            let past = input[0];
            out.head[past] = 77u;
            out.rest[0] = out.head[past - 1u] + 1u;
        }
    ";
    // head[5] is byte 20, rest[1]; head[4] is rest[0], read before it is written.
    let gpu = Gpu::new();
    for module in [
        admitted(source, &Options::default()),
        guarded(admitted(source, &Options::default())),
    ] {
        let out = gpu.compute(module, &[5], 1, 8);
        assert_eq!(
            (out[5], out[4]),
            (77, 1),
            "a buffer is bounded by its binding alone"
        );
    }
}
