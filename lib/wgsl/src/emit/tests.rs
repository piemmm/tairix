//! The emitter held to admission: every module it writes is one the
//! validator admits, deterministically.

use alloc::string::String;
use alloc::vec::Vec;

use crate::diag::Stop;
use crate::{compile, Options};

/// The module `source` compiles to, admitted; a refusal names the rule and
/// the instruction the validator stopped at.
fn compiled(source: &str) -> Vec<u8> {
    let options = Options::default();
    let stop = |stop: Stop| -> String {
        match stop {
            Stop::Error(code, span, message) => {
                alloc::format!("refused at {span:?} with {code:?}: {message}")
            }
            other => alloc::format!("{other:?}"),
        }
    };
    let tokens = crate::lex::tokens(source)
        .map_err(stop)
        .unwrap_or_else(|e| panic!("{e}\n{source}"));
    let tree = crate::parse::parse(source, &tokens)
        .map_err(stop)
        .unwrap_or_else(|e| panic!("{e}\n{source}"));
    let symbols =
        || crate::scope::Symbols::intern(source, &tokens).unwrap_or_else(|_| unreachable!());
    let mut checker = crate::check::check(source, &tree, symbols(), &options)
        .map_err(stop)
        .unwrap_or_else(|e| panic!("{e}\n{source}"));
    let builder = super::write(&mut checker)
        .map_err(stop)
        .unwrap_or_else(|e| panic!("{e}\n{source}"));
    if let Err(refusal) = builder.finish() {
        let mut again = crate::check::check(source, &tree, symbols(), &options)
            .unwrap_or_else(|_| unreachable!());
        let laid = super::write(&mut again)
            .unwrap_or_else(|_| unreachable!())
            .lay_out_unvalidated()
            .unwrap_or_else(|e| panic!("{e:?}"));
        panic!(
            "the validator refused the module: {refusal:?}\n{}\n{source}",
            tairix_spirv::assemble::listing(&laid)
        );
    }
    let compiled = compile(source, &options).unwrap_or_else(|f| panic!("{f:?}"));
    compiled.module
}

#[test]
fn a_compute_shader_is_written_and_admitted() {
    compiled(
        "
        @group(0) @binding(0) var<storage, read_write> out: array<u32>;
        @compute @workgroup_size(4)
        fn main(@builtin(global_invocation_id) id: vec3u) {
            out[id.x] = id.x * 2u + 1u;
        }
        ",
    );
}

#[test]
fn a_render_pipeline_is_written_and_admitted() {
    compiled(crate::check::tests::TRIANGLE);
}

#[test]
fn control_flow_of_every_kind_is_written_structured() {
    compiled(
        "
        @group(0) @binding(0) var<storage, read_write> out: array<i32>;
        fn pick(x: i32) -> i32 {
            if x < 0 { return -1; } else if x == 0 { return 0; } else if x < 10 { return 1; }
            return 2;
        }
        @compute @workgroup_size(1)
        fn main(@builtin(global_invocation_id) id: vec3u) {
            var total = 0;
            for (var i = 0; i < 10; i++) {
                if i == 3 { continue; }
                if i == 8 { break; }
                total += i;
            }
            var j = 0;
            while j < 5 { j += 1; }
            loop {
                j -= 1;
                if j == 2 { continue; }
                continuing { break if j <= 0; }
            }
            switch i32(id.x) {
                case 0, 1: { total += 1; }
                case 2: { total += 2; break; }
                default: { total -= 1; }
            }
            out[id.x] = total + pick(j);
        }
        ",
    );
}

#[test]
fn arithmetic_wgsl_defines_and_spirv_leaves_undefined_is_written_out() {
    compiled(
        "
        @group(0) @binding(0) var<storage, read_write> ints: array<i32>;
        @group(0) @binding(1) var<storage, read_write> uints: array<u32>;
        @group(0) @binding(2) var<storage, read_write> floats: array<f32>;
        @compute @workgroup_size(1)
        fn main() {
            let a = ints[0];
            let b = ints[1];
            ints[2] = a / b;
            ints[3] = a % b;
            ints[4] = a << uints[0];
            ints[5] = a >> uints[1];
            uints[2] = uints[0] / uints[1];
            uints[3] = uints[0] % uints[1];
            let v = vec3i(a, b, 3) / vec3i(b);
            ints[6] = v.x + v.y + v.z;
            ints[7] = i32(floats[0]);
            uints[4] = u32(floats[1]);
            floats[2] = f32(a) % floats[1];
            ints[8] = -a;
            ints[9] = ~a;
            uints[5] = u32(a > b && floats[0] != floats[1]);
        }
        ",
    );
}

#[test]
fn numeric_built_ins_are_written_and_admitted() {
    compiled(
        "
        @group(0) @binding(0) var<storage, read_write> f: array<vec4f>;
        @group(0) @binding(1) var<storage, read_write> i: array<vec4i>;
        @group(0) @binding(2) var<storage, read_write> u: array<vec4u>;
        @compute @workgroup_size(1)
        fn main() {
            let x = f[0];
            let y = f[1];
            f[2] = abs(x) + acos(x) + acosh(x) + asin(x) + asinh(x) + atan(x) + atanh(x) + atan2(x, y);
            f[3] = ceil(x) + clamp(x, y, x) + cos(x) + cosh(x) + degrees(x) + exp(x) + exp2(x);
            f[4] = floor(x) + fma(x, y, x) + fract(x) + inverseSqrt(x) + log(x) + log2(x);
            f[5] = max(x, y) + min(x, y) + mix(x, y, 0.5) + mix(x, y, y) + normalize(x) + pow(x, y);
            f[6] = radians(x) + round(x) + saturate(x) + sign(x) + sin(x) + sinh(x) + smoothstep(x, y, x);
            f[7] = sqrt(x) + step(x, y) + tan(x) + tanh(x) + trunc(x) + reflect(x, y) + refract(x, y, 1.5);
            f[8] = vec4f(dot(x, y), length(x), distance(x, y), determinant(mat2x2f(x.xy, y.xy)));
            f[9] = vec4f(cross(x.xyz, y.xyz), quantizeToF16(x.w));
            f[10] = faceForward(x, y, x) + ldexp(x, vec4i(2)) + frexp(x).fract + modf(x).whole;
            f[11] = select(x, y, x < y) + select(x, y, true) + vec4f(f32(all(x < y)), f32(any(x > y)), 0.0, 0.0);
            let m = mat4x4f(x, y, x, y);
            f[12] = transpose(m) * x + x * m;
            let a = i[0];
            let b = i[1];
            i[2] = abs(a) + clamp(a, b, a) + max(a, b) + min(a, b) + sign(a);
            i[3] = vec4i(dot(a, b)) + countLeadingZeros(a) + countOneBits(a) + countTrailingZeros(a);
            i[4] = firstLeadingBit(a) + firstTrailingBit(a) + reverseBits(a) + extractBits(a, 3u, 4u);
            i[5] = insertBits(a, b, 2u, 5u) + frexp(x).exp;
            let c = u[0];
            let d = u[1];
            u[2] = clamp(c, d, c) + max(c, d) + min(c, d) + countLeadingZeros(c) + firstLeadingBit(c);
            u[3] = extractBits(c, 1u, 30u) + insertBits(c, d, 31u, 1u) + vec4u(dot(c, d));
            u[4] = vec4u(dot4U8Packed(c.x, d.x), u32(dot4I8Packed(c.y, d.y)), bitcast<u32>(x.z), bitcast<u32>(a.w));
        }
        ",
    );
}

#[test]
fn packing_built_ins_are_written_and_admitted() {
    compiled(
        "
        @group(0) @binding(0) var<storage, read_write> f: array<vec4f>;
        @group(0) @binding(1) var<storage, read_write> u: array<u32>;
        @group(0) @binding(2) var<storage, read_write> i: array<vec4i>;
        @compute @workgroup_size(1)
        fn main() {
            let x = f[0];
            u[0] = pack4x8snorm(x);
            u[1] = pack4x8unorm(x);
            u[2] = pack2x16snorm(x.xy);
            u[3] = pack2x16unorm(x.zw);
            u[4] = pack2x16float(x.xy);
            u[5] = pack4xI8(i[0]);
            u[6] = pack4xU8(vec4u(i[0]));
            u[7] = pack4xI8Clamp(i[1]);
            u[8] = pack4xU8Clamp(vec4u(i[1]));
            f[1] = unpack4x8snorm(u[0]) + unpack4x8unorm(u[1]);
            f[2] = vec4f(unpack2x16snorm(u[2]), unpack2x16unorm(u[3]));
            f[3] = vec4f(unpack2x16float(u[4]), 0.0, 0.0);
            i[2] = unpack4xI8(u[5]) + vec4i(unpack4xU8(u[6]));
        }
        ",
    );
}

#[test]
fn pointers_are_passed_as_their_roots_and_indexes() {
    compiled(
        "
        struct Item { weight: f32, tags: array<u32, 4> }
        struct Store { count: u32, items: array<Item> }
        @group(0) @binding(0) var<storage, read_write> store: Store;
        var<private> scratch: array<Item, 8>;
        var<workgroup> shared_items: array<Item, 16>;
        fn bump(p: ptr<function, u32>) { *p += 1u; }
        fn weigh(item: ptr<storage, Item, read_write>, by: f32) { (*item).weight *= by; item.tags[2] = 7u; }
        fn tag(t: ptr<private, u32>) { *t = 3u; }
        fn first(t: ptr<workgroup, array<u32, 4>>) -> u32 { return t[0]; }
        @compute @workgroup_size(8)
        fn main(@builtin(local_invocation_index) lane: u32) {
            var n = 0u;
            bump(&n);
            let p = &n;
            bump(p);
            weigh(&store.items[lane], 2.0);
            weigh(&store.items[n], 0.5);
            tag(&scratch[lane].tags[1]);
            store.count = n + arrayLength(&store.items) + first(&shared_items[lane].tags);
        }
        ",
    );
}

#[test]
fn a_fragment_shader_that_discards_masks_its_writes_and_ends_where_it_returns() {
    compiled(
        "
        @group(0) @binding(0) var<storage, read_write> seen: array<atomic<u32>, 4>;
        @group(0) @binding(1) var<storage, read_write> plain: array<u32, 4>;
        @fragment
        fn main(@builtin(position) at: vec4f, @location(0) @interpolate(flat) id: u32) -> @location(0) vec4f {
            if at.x < 4.0 { discard; }
            plain[id % 4u] = 1u;
            let before = atomicAdd(&seen[id % 4u], 1u);
            atomicStore(&seen[0], before);
            let swapped = atomicCompareExchangeWeak(&seen[1], 0u, 1u);
            return vec4f(f32(before), f32(swapped.exchanged), dpdx(at.x), fwidth(at.y));
        }
        ",
    );
}

#[test]
fn workgroup_memory_is_zeroed_and_barriers_written() {
    compiled(
        "
        var<workgroup> tile: array<f32, 300>;
        var<workgroup> total: atomic<u32>;
        var<workgroup> flag: u32;
        @group(0) @binding(0) var<storage, read_write> out: array<f32>;
        @compute @workgroup_size(8, 4)
        fn main(@builtin(local_invocation_index) lane: u32,
                @builtin(workgroup_index) group: u32,
                @builtin(global_invocation_index) global: u32) {
            tile[lane] = f32(lane);
            atomicAdd(&total, 1u);
            workgroupBarrier();
            storageBarrier();
            textureBarrier();
            let all = workgroupUniformLoad(&flag);
            let counted = workgroupUniformLoad(&total);
            out[global] = tile[(lane + 1u) % 32u] + f32(counted + all + group);
        }
        ",
    );
}

#[test]
fn textures_of_every_kind_are_sampled_loaded_queried_and_stored() {
    compiled(
        "
        @group(0) @binding(0) var colour: texture_2d<f32>;
        @group(0) @binding(1) var layers: texture_2d_array<f32>;
        @group(0) @binding(2) var cube: texture_cube<f32>;
        @group(0) @binding(3) var cubes: texture_cube_array<f32>;
        @group(0) @binding(4) var depth: texture_depth_2d;
        @group(0) @binding(5) var depths: texture_depth_2d_array;
        @group(0) @binding(6) var volume: texture_3d<f32>;
        @group(0) @binding(7) var line: texture_1d<f32>;
        @group(0) @binding(8) var counts: texture_2d<u32>;
        @group(0) @binding(9) var many: texture_multisampled_2d<f32>;
        @group(0) @binding(10) var outside: texture_external;
        @group(0) @binding(11) var picture: texture_storage_2d<rgba8unorm, write>;
        @group(0) @binding(12) var grid: texture_storage_2d_array<r32uint, read_write>;
        @group(1) @binding(0) var smooth_sampler: sampler;
        @group(1) @binding(1) var shadow: sampler_comparison;
        fn through(t: texture_2d<f32>, s: sampler, at: vec2f) -> vec4f {
            return textureSample(t, s, at);
        }
        @fragment
        fn main(@location(0) uv: vec2f) -> @location(0) vec4f {
            var c = textureSample(colour, smooth_sampler, uv);
            c += textureSample(colour, smooth_sampler, uv, vec2i(1, -1));
            c += textureSample(layers, smooth_sampler, uv, 2);
            c += textureSample(cube, smooth_sampler, vec3f(uv, 1.0));
            c += textureSample(cubes, smooth_sampler, vec3f(uv, 1.0), 1u);
            c += textureSample(volume, smooth_sampler, vec3f(uv, 0.5));
            c += textureSample(line, smooth_sampler, uv.x);
            c += textureSampleBias(colour, smooth_sampler, uv, 0.5);
            c += textureSampleGrad(colour, smooth_sampler, uv, vec2f(0.1), vec2f(0.2));
            c += textureSampleLevel(colour, smooth_sampler, uv, 1.0);
            c += textureSampleBaseClampToEdge(outside, smooth_sampler, uv);
            c += textureGather(1, colour, smooth_sampler, uv);
            c += textureGather(depth, smooth_sampler, uv);
            c += textureGatherCompare(depth, shadow, uv, 0.5);
            let d = textureSample(depth, smooth_sampler, uv) + textureSampleLevel(depths, smooth_sampler, uv, 1, 0)
                + textureSampleCompare(depth, shadow, uv, 0.5)
                + textureSampleCompareLevel(depths, shadow, uv, 2, 0.5);
            c += textureLoad(colour, vec2i(1, 2), 0) + textureLoad(layers, vec2u(1u), 1, 0);
            c += textureLoad(many, vec2i(0), 3) + textureLoad(outside, vec2u(4u));
            c += vec4f(textureLoad(counts, vec2i(0), 0)) + vec4f(textureLoad(grid, vec2i(1), 2));
            c += vec4f(textureLoad(depth, vec2i(0), 0));
            c += through(colour, smooth_sampler, uv);
            let size = textureDimensions(colour) + textureDimensions(colour, 1) + textureDimensions(cubes);
            let other = textureDimensions(volume).x + textureDimensions(line) + textureDimensions(many).x;
            let levels = textureNumLevels(colour) + textureNumLayers(layers) + textureNumSamples(many)
                + textureNumLayers(grid) + textureDimensions(picture).x + textureDimensions(outside).y;
            textureStore(picture, vec2i(0), c);
            textureStore(grid, vec2u(1u), 3, vec4u(levels));
            return c + vec4f(d) + vec4f(vec2f(size), f32(other), 0.0);
        }
        ",
    );
}

#[test]
fn the_stage_interface_is_written_with_its_built_ins_and_interpolation() {
    compiled(
        "
        enable dual_source_blending;
        struct Varyings {
            @builtin(position) @invariant position: vec4f,
            @location(0) @interpolate(linear, centroid) uv: vec2f,
            @location(1) @interpolate(perspective, sample) shade: f32,
            @location(2) @interpolate(flat) id: u32,
            @location(3) @interpolate(flat, either) layer: i32,
        }
        struct Blend {
            @location(0) @blend_src(0) colour: vec4f,
            @location(0) @blend_src(1) factor: vec4f,
            @builtin(frag_depth) depth: f32,
            @builtin(sample_mask) mask: u32,
        }
        @vertex
        fn vs(@builtin(vertex_index) v: u32, @builtin(instance_index) n: u32) -> Varyings {
            return Varyings(vec4f(f32(v), f32(n), 0.0, 1.0), vec2f(0.5), 1.0, v, i32(n));
        }
        @fragment
        fn fs(in: Varyings, @builtin(front_facing) front: bool, @builtin(sample_index) s: u32,
              @builtin(sample_mask) mask: u32) -> Blend {
            let shade = select(in.shade, -in.shade, front);
            return Blend(vec4f(in.uv, shade, 1.0), vec4f(f32(in.id + s)), in.position.z, mask);
        }
        ",
    );
}

#[test]
fn overrides_are_written_as_the_constants_the_compile_gives_them() {
    let source = "
        override width: u32 = 8;
        @id(7) override scale: f32;
        override doubled = width * 2u;
        var<workgroup> row: array<f32, doubled>;
        @group(0) @binding(0) var<storage, read_write> out: array<f32>;
        @compute @workgroup_size(width, 2)
        fn main(@builtin(local_invocation_index) lane: u32) {
            row[lane] = scale;
            workgroupBarrier();
            out[lane] = row[(lane + 1u) % doubled] * scale;
        }
    ";
    let options = Options {
        overrides: &[(crate::OverrideKey::Id(7), 2.5)],
    };
    let first = compile(source, &options).unwrap_or_else(|f| panic!("{f:?}"));
    let again = compile(source, &options).unwrap_or_else(|f| panic!("{f:?}"));
    assert_eq!(first.module, again.module, "a compile is deterministic");
}

#[test]
fn assignments_write_through_places_lanes_and_compound_operators() {
    compiled(
        "
        requires swizzle_assignment;
        struct Body { position: vec4f, angles: array<vec3f, 3> }
        @group(0) @binding(0) var<storage, read_write> bodies: array<Body>;
        var<private> spin: mat3x3f;
        @compute @workgroup_size(1)
        fn main() {
            var b = bodies[0];
            b.position.xz = vec2f(1.0, 2.0);
            b.position.yw += vec2f(3.0);
            b.angles[1].y -= 0.5;
            b.angles[2] *= 2.0;
            spin[1] = b.angles[0];
            spin[2].x = 4.0;
            var counter = 0u;
            counter++;
            counter--;
            counter <<= 2u;
            counter |= 1u;
            _ = counter;
            bodies[1] = b;
            bodies[2].angles = b.angles;
            bodies[3].position = vec4f(spin * b.angles[0], 1.0);
        }
        ",
    );
}

#[test]
fn a_short_circuit_with_effects_runs_its_right_side_only_where_the_left_does_not_decide() {
    compiled(
        "
        @group(0) @binding(0) var<storage, read_write> hits: atomic<u32>;
        @group(0) @binding(1) var<storage, read_write> out: array<u32>;
        fn touch() -> bool { atomicAdd(&hits, 1u); return true; }
        @compute @workgroup_size(1)
        fn main(@builtin(global_invocation_id) id: vec3u) {
            let a = id.x > 2u && touch();
            let b = id.x == 0u || (touch() && id.y < 4u);
            out[id.x] = u32(a) + u32(b);
        }
        ",
    );
}

#[test]
fn f16_values_are_held_in_buffers_and_converted() {
    compiled(
        "
        enable f16;
        struct Half { a: f16, b: vec3h, m: mat2x2h }
        @group(0) @binding(0) var<storage, read_write> halves: array<Half>;
        @group(0) @binding(1) var<uniform> scale: vec4h;
        @compute @workgroup_size(1)
        fn main() {
            let h = halves[0];
            halves[1].a = h.a * scale.x + f16(2.5);
            halves[1].b = h.b * h.m[0].x;
            halves[2].m = h.m * scale.y;
            let back = f32(h.a) + f32(i32(h.b.x)) + f32(u32(scale.w));
            halves[3].a = f16(back);
        }
        ",
    );
}

#[test]
fn values_are_indexed_dynamically_through_memory_and_by_lane() {
    compiled(
        "
        @group(0) @binding(0) var<storage, read_write> out: array<f32>;
        const table = array(1.0, 2.0, 4.0, 8.0);
        @compute @workgroup_size(1)
        fn main(@builtin(global_invocation_id) id: vec3u) {
            let i = id.x % 4u;
            let m = mat2x2f(1.0, 2.0, 3.0, 4.0);
            let v = vec4f(9.0, 8.0, 7.0, 6.0);
            let local = array(m, m * 2.0);
            out[i] = table[i] + v[i] + m[i % 2u][i % 2u] + local[i % 2u][1].y;
        }
        ",
    );
}

#[test]
fn a_module_with_no_entry_point_is_refused_as_holding_nothing_to_run() {
    let source = "fn helper(x: u32) -> u32 { return x * 2u; }";
    match compile(source, &Options::default()) {
        Err(crate::Failure::Error(diagnostic)) => {
            assert_eq!(diagnostic.code, crate::Code::Unsupported);
        }
        other => panic!("{other:?}"),
    }
}
