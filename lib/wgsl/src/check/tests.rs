//! The checker held to the specification: modules it must take, and the
//! code each module it must refuse is refused with.

use alloc::string::String;

use crate::diag::{Code, Stop};
use crate::{Options, OverrideKey};

/// What checking `source` with `overrides` answers.
fn check_with(source: &str, overrides: &[(OverrideKey<'_>, f64)]) -> Result<(), (Code, String)> {
    let stop = |stop: Stop| match stop {
        Stop::Error(code, _, message) => (code, message),
        Stop::OutOfMemory => panic!("out of memory checking {source}"),
        Stop::Internal => panic!("an internal invariant failed checking {source}"),
    };
    let tokens = crate::lex::tokens(source).map_err(stop)?;
    let tree = crate::parse::parse(source, &tokens).map_err(stop)?;
    let symbols = crate::scope::Symbols::intern(source, &tokens).map_err(stop)?;
    let options = Options { overrides };
    super::check(source, &tree, symbols, &options)
        .map(|_| ())
        .map_err(stop)
}

fn accepted(source: &str) {
    if let Err((code, message)) = check_with(source, &[]) {
        panic!("refused with {code:?}: {message}\n{source}");
    }
}

fn refused(source: &str) -> Code {
    match check_with(source, &[]) {
        Ok(()) => panic!("accepted\n{source}"),
        Err((code, _)) => code,
    }
}

pub(crate) const TRIANGLE: &str = "
struct Out {
    @builtin(position) position: vec4f,
    @location(0) colour: vec3f,
    @location(1) @interpolate(flat) id: u32,
}

struct Frame { scale: vec2f, tint: vec4f }
@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var picture: texture_2d<f32>;
@group(0) @binding(2) var linear_sampler: sampler;

@vertex
fn vs(@builtin(vertex_index) index: u32) -> Out {
    var corners = array(vec2f(0.0, 0.5), vec2f(-0.5, -0.5), vec2f(0.5, -0.5));
    var out: Out;
    out.position = vec4f(corners[index] * frame.scale, 0.0, 1.0);
    out.colour = vec3f(f32(index) / 2.0);
    out.id = index;
    return out;
}

@fragment
fn fs(in: Out) -> @location(0) vec4f {
    let texel = textureSample(picture, linear_sampler, in.position.xy / 64.0);
    return vec4f(in.colour, 1.0) * texel * frame.tint;
}
";

#[test]
fn a_render_pipeline_checks() {
    accepted(TRIANGLE);
}

#[test]
fn a_compute_shader_with_workgroup_memory_and_atomics_checks() {
    accepted(
        "
        struct Buffer { count: atomic<u32>, values: array<f32> }
        @group(0) @binding(0) var<storage, read_write> data: Buffer;
        var<workgroup> tile: array<f32, 64>;
        override width: u32 = 64;

        @compute @workgroup_size(width)
        fn main(@builtin(local_invocation_index) local: u32, @builtin(global_invocation_id) id: vec3u) {
            tile[local] = data.values[id.x];
            workgroupBarrier();
            if local == 0u {
                var total = 0.0;
                for (var i = 0u; i < 64u; i++) {
                    total += tile[i];
                }
                data.values[id.x] = total;
                atomicAdd(&data.count, 1u);
            }
            _ = arrayLength(&data.values);
        }
        ",
    );
}

#[test]
fn constants_evaluate_as_the_specification_has_them() {
    accepted(
        "
        const a = 1 + 2 * 3;
        const_assert a == 7;
        const b: i32 = 2147483647i + 1i;
        const_assert b == -2147483647i - 1i;
        const c = vec3(1, 2, 3).zyx;
        const_assert c.x == 3 && c.z == 1;
        const d = array(1.5, 2.5)[1];
        const_assert d == 2.5;
        const e = sqrt(16.0) + abs(-2.0) + max(1, 3);
        const_assert e == 9.0;
        const f = mat2x2(1.0, 2.0, 3.0, 4.0) * vec2(1.0, 1.0);
        const_assert f.x == 4.0 && f.y == 6.0;
        const g = u32(4 * 1000 * 1000 * 1000);
        const_assert g == 4000000000u;
        const h = select(1, 2, true) + i32(3.9) + i32(countOneBits(7u)) + i32(firstLeadingBit(1u));
        const_assert h == 2 + 3 + 3 + 0;
        const k = frexp(1.5).fract + modf(2.25).fract;
        const_assert k == 1.0;
        const l = bitcast<u32>(1.0f);
        const_assert l == 0x3f800000u;
        const m = pack4x8unorm(vec4(1.0, 0.0, 0.0, 1.0));
        const_assert m == 0xff0000ffu;
        const n = determinant(mat2x2(2.0, 0.0, 0.0, 3.0)) + dot(vec2(1, 2), vec2(3, 4));
        const_assert n == 17.0;
        ",
    );
}

#[test]
fn a_constant_short_circuit_leaves_its_right_side_unevaluated() {
    accepted(
        "const x = false && (1.0e300 * 1.0e300 > 0.0); const y = true || (9223372036854775807 + 1 == 0);",
    );
    assert_eq!(
        refused("const y = true || (1 / 0 == 0);"),
        Code::Evaluation,
        "a constant divisor of zero is refused even where a short-circuit skips it"
    );
}

#[test]
fn constant_evaluation_errors_are_refused() {
    assert_eq!(refused("const x = 1 / 0;"), Code::Evaluation);
    assert_eq!(refused("const x = 1.0e300 * 1.0e300;"), Code::Evaluation);
    assert_eq!(
        refused("const x = 9223372036854775807 + 1;"),
        Code::Evaluation
    );
    assert_eq!(refused("const x = sqrt(-1.0);"), Code::Evaluation);
    assert_eq!(refused("const x = array(1, 2)[2];"), Code::Evaluation);
    assert_eq!(refused("const x = 1u << 32u;"), Code::Evaluation);
    assert_eq!(refused("const_assert 1 == 2;"), Code::Assertion);
    assert_eq!(refused("const x = clamp(1, 3, 2);"), Code::Evaluation);
    assert_eq!(
        refused("const x = extractBits(1u, 30u, 3u);"),
        Code::Evaluation
    );
}

#[test]
fn a_constant_divisor_of_zero_is_refused_whatever_the_dividend() {
    assert_eq!(
        refused("fn f(x: i32) -> i32 { return x / 0; }"),
        Code::Evaluation
    );
    assert_eq!(
        refused("fn f(x: vec2<u32>) -> vec2<u32> { return x % vec2(1u, 0u); }"),
        Code::Evaluation
    );
}

#[test]
fn names_resolve_in_scope() {
    assert_eq!(refused("fn f() { let x = y; }"), Code::Undeclared);
    assert_eq!(
        refused("fn f() { let x = 1; let x = 2; }"),
        Code::Redeclared
    );
    assert_eq!(refused("fn f(a: i32) { let a = 1; }"), Code::Redeclared);
    assert_eq!(refused("const a = 1; const a = 2;"), Code::Redeclared);
    assert_eq!(refused("const a = b; const b = a;"), Code::Cycle);
    accepted("fn f() { let x = 1; { let x = 2.0; _ = x; } _ = x; }");
    accepted("const f32 = 1; fn g() -> i32 { return f32; }");
    accepted("fn f() -> i32 { return g(); } fn g() -> i32 { return 1; }");
}

#[test]
fn recursion_is_refused() {
    assert_eq!(refused("fn f() { g(); } fn g() { f(); }"), Code::Cycle);
    assert_eq!(refused("fn f() { f(); }"), Code::Cycle);
}

#[test]
fn types_are_held_to_their_rules() {
    assert_eq!(refused("fn f() { var x: i32 = 1.5; }"), Code::Mismatch);
    assert_eq!(refused("fn f() { let x = 1u + 1i; }"), Code::Mismatch);
    assert_eq!(
        refused("fn f() { let x = vec2<bool>(true, false) + vec2(true, true); }"),
        Code::Mismatch
    );
    assert_eq!(refused("var<private> x: array<f32>;"), Code::AddressSpace);
    assert_eq!(refused("var<private> x: atomic<u32>;"), Code::AddressSpace);
    assert_eq!(refused("fn f() { let x = sqrt(1i); }"), Code::NoOverload);
    assert_eq!(
        refused("fn f() { let x = vec3<f32>(1.0, 2.0); }"),
        Code::NoOverload
    );
    assert_eq!(refused("struct S { a: array<f32>, b: f32 }"), Code::Type);
    assert_eq!(refused("alias A = array<f32, 0>;"), Code::Type);
    assert_eq!(refused("fn f() { let h = 1.0h; }"), Code::Type);
    accepted("enable f16; fn f() { let h = 1.0h + f16(2); _ = h; }");
}

#[test]
fn functions_return_on_every_path() {
    assert_eq!(
        refused("fn f(c: bool) -> i32 { if c { return 1; } }"),
        Code::Behaviour
    );
    assert_eq!(refused("fn f() { loop { } }"), Code::Behaviour);
    accepted("fn f(c: bool) -> i32 { if c { return 1; } else { return 2; } }");
    accepted("fn f() -> i32 { loop { return 1; } }");
    accepted(
        "fn f(c: i32) -> i32 { switch c { case 1, 2: { return 1; } default: { return 0; } } }",
    );
}

#[test]
fn control_flow_statements_stand_where_they_may() {
    assert_eq!(refused("fn f() { break; }"), Code::Behaviour);
    assert_eq!(refused("fn f() { continue; }"), Code::Behaviour);
    assert_eq!(
        refused("fn f() { loop { continuing { break; } } }"),
        Code::Behaviour
    );
    assert_eq!(
        refused("fn f() -> i32 { loop { continuing { return 1; } } }"),
        Code::Behaviour
    );
    assert_eq!(
        refused("fn f() { var i = 0; loop { if i > 3 { continue; } let x = i; continuing { i = x + 1; break if i > 9; } } }"),
        Code::Behaviour,
        "a continue skips a declaration the continuing block uses"
    );
    accepted("fn f() { var i = 0; loop { let x = i; if x > 3 { continue; } continuing { i = x + 1; break if i > 9; } } }");
    accepted(
        "fn f() { for (var i = 0; i < 4; i++) { if i == 2 { continue; } } while true { break; } }",
    );
    assert_eq!(
        refused("fn f(c: i32) { switch c { case 1: { } case 1: { } default: { } } }"),
        Code::Evaluation
    );
    assert_eq!(
        refused("fn f(c: i32) { switch c { case 1: { } } }"),
        Code::Syntax
    );
    accepted(
        "fn f(c: i32, d: u32) { switch c { case 1: { switch d { case 1u: { } default: { } } }
         default: { loop { switch d { default: { break; } } break; } } } }",
    );
}

#[test]
fn a_value_s_result_is_used_where_its_function_requires() {
    assert_eq!(refused("fn f() { sqrt(1.0); }"), Code::Misused);
    assert_eq!(refused("fn f() { vec2f(); }"), Code::Misused);
    assert_eq!(
        refused("@must_use fn g() -> i32 { return 1; } fn f() { g(); }"),
        Code::Misused
    );
    accepted("fn g() -> i32 { return 1; } fn f() { g(); _ = sqrt(1.0); }");
}

#[test]
fn assignments_write_memory_they_may() {
    assert_eq!(refused("fn f() { let x = 1; x = 2; }"), Code::Pointer);
    assert_eq!(
        refused("@group(0) @binding(0) var<uniform> u: vec4f; fn f() { u = vec4f(); }"),
        Code::Pointer
    );
    assert_eq!(
        refused("fn f() { var v = vec3f(); v.xx = vec2f(); }"),
        Code::Pointer
    );
    accepted(
        "fn f() { var v = vec3f(); v.xz = vec2f(1.0, 2.0); v.y += 1.0; var i = 1; i++; i <<= 2u; }",
    );
    accepted(
        "fn g(p: ptr<function, i32>) { *p = 3; } fn f() { var x = 1; g(&x); let p = &x; *p += 1; }",
    );
}

#[test]
fn entry_points_meet_their_stage() {
    assert_eq!(
        refused("@vertex fn vs() -> @location(0) vec4f { return vec4f(); }"),
        Code::Interface,
        "a vertex shader returns position"
    );
    assert_eq!(
        refused("@vertex fn vs() -> @builtin(position) vec4f { discard; return vec4f(); }"),
        Code::Interface
    );
    assert_eq!(
        refused(
            "@group(0) @binding(0) var t: texture_2d<f32>; @group(0) @binding(1) var s: sampler;
                 @compute @workgroup_size(1) fn c() { _ = textureSample(t, s, vec2f()); }"
        ),
        Code::Interface
    );
    assert_eq!(
        refused("var<workgroup> w: i32; @fragment fn fs() { w = 1; }"),
        Code::Interface
    );
    assert_eq!(
        refused(
            "@group(0) @binding(0) var<storage, read_write> b: array<f32>;
                 @vertex fn vs() -> @builtin(position) vec4f { b[0] = 1.0; return vec4f(); }"
        ),
        Code::Interface
    );
    assert_eq!(
        refused("@group(0) @binding(0) var<uniform> a: vec4f; @group(0) @binding(0) var<uniform> b: vec4f;
                 @fragment fn fs() -> @location(0) vec4f { return a + b; }"),
        Code::Interface
    );
    accepted(
        "@group(0) @binding(0) var<uniform> a: vec4f; @group(0) @binding(0) var<uniform> b: vec4f;
              @fragment fn fs() -> @location(0) vec4f { return a; }
              @fragment fn gs() -> @location(0) vec4f { return b; }",
    );
    assert_eq!(
        refused(
            "@vertex fn vs() -> @builtin(position) vec4f { return vec4f(); }
                 @fragment fn fs(@location(0) n: u32) {}"
        ),
        Code::Interface,
        "an integer between stages is flat"
    );
    assert_eq!(
        refused("@fragment fn fs(@builtin(position) p: vec3f) {}"),
        Code::Mismatch
    );
    assert_eq!(
        refused("@compute fn c() {}"),
        Code::Attribute,
        "a compute shader declares its workgroup size"
    );
    assert_eq!(
        refused("fn helper() {} @fragment fn fs() { helper(); fs(); }"),
        Code::Misused
    );
}

#[test]
fn layout_constraints_bind_each_address_space() {
    assert_eq!(
        refused("@group(0) @binding(0) var<uniform> u: array<f32, 4>;"),
        Code::Layout,
        "a uniform array's stride is 16"
    );
    accepted("@group(0) @binding(0) var<uniform> u: array<vec4f, 4>;");
    assert_eq!(refused("struct S { @size(2) a: f32 }"), Code::Layout);
    assert_eq!(refused("struct S { @align(3) a: f32 }"), Code::Attribute);
    assert_eq!(
        refused(
            "struct S { a: f32, @location(0) b: vec4f, @location(0) c: vec4f }
                 @fragment fn fs() -> S { return S(); }"
        ),
        Code::Interface
    );
    assert_eq!(
        refused(
            "struct A { x: f32 } struct S { a: A, b: f32 }
                 @group(0) @binding(0) var<uniform> u: S;"
        ),
        Code::Layout,
        "a uniform structure's nested structure takes 16 bytes"
    );
    accepted(
        "struct A { x: f32 } struct S { a: A, @align(16) b: f32 }
              @group(0) @binding(0) var<uniform> u: S;",
    );
    assert_eq!(
        refused(
            "struct Item { weight: f32, tags: array<u32, 4294967295u> }
                 @group(0) @binding(0) var<storage> items: array<Item>;"
        ),
        Code::Layout,
        "an element of more than 2^32 bytes has a stride no module states"
    );
    accepted("@group(0) @binding(0) var<storage> words: array<u32, 4294967295u>;");
}

#[test]
fn an_override_is_evaluated_only_where_an_entry_point_needs_it() {
    let needs_b = "override a: i32 = 0; override b = 1 / a;
                   @fragment fn frag1() { _ = b; }";
    assert_eq!(
        check_with(needs_b, &[]).map_err(|e| e.0),
        Err(Code::Evaluation)
    );
    assert_eq!(
        check_with(needs_b, &[(OverrideKey::Name("b"), 3.0)]),
        Ok(()),
        "b overridden"
    );
    assert_eq!(
        check_with(needs_b, &[(OverrideKey::Name("a"), 2.0)]),
        Ok(()),
        "a overridden"
    );
    let unreached = "override a: i32 = 0; override b = 1 / a; @fragment fn frag2() {}";
    assert_eq!(check_with(unreached, &[]), Ok(()), "b is no part of frag2");
    assert_eq!(
        refused("override a: i32 = 0; override b = a / 0;"),
        Code::Evaluation,
        "a constant divisor of zero is a shader-creation error"
    );
    assert_eq!(
        check_with(
            "override n: u32; @compute @workgroup_size(n) fn c() {}",
            &[]
        )
        .map_err(|e| e.0),
        Err(Code::OverrideValue)
    );
    assert_eq!(
        check_with(
            "override n: u32; @compute @workgroup_size(n) fn c() {}",
            &[(OverrideKey::Name("n"), 8.0)]
        ),
        Ok(())
    );
    assert_eq!(
        check_with("@id(7) override n: u32 = 1;", &[(OverrideKey::Id(7), 2.5)]),
        Ok(()),
        "a pipeline constant is truncated toward zero"
    );
    assert_eq!(
        check_with("override n: u32 = 1;", &[(OverrideKey::Name("m"), 2.0)]).map_err(|e| e.0),
        Err(Code::OverrideValue),
        "a value for no override"
    );
    assert_eq!(
        check_with("override n: u32 = 1;", &[(OverrideKey::Name("n"), -1.0)]).map_err(|e| e.0),
        Err(Code::OverrideValue)
    );
}

#[test]
fn an_override_sized_array_is_workgroup_memory_with_a_positive_count() {
    let source = "override n: u32 = 4; var<workgroup> w: array<f32, n>;
                  @compute @workgroup_size(1) fn c() { w[0] = 1.0; }";
    assert_eq!(check_with(source, &[]), Ok(()));
    assert_eq!(
        check_with(source, &[(OverrideKey::Name("n"), 0.0)]).map_err(|e| e.0),
        Err(Code::Type)
    );
    assert_eq!(
        check_with(source, &[(OverrideKey::Name("n"), 5000.0)]).map_err(|e| e.0),
        Err(Code::MemoryLimit),
        "past the workgroup memory limit"
    );
    assert_eq!(
        refused("override n: u32 = 4; var<private> p: array<f32, n>;"),
        Code::AddressSpace
    );
}

#[test]
fn extensions_are_offered_or_refused_by_name() {
    assert_eq!(refused("enable subgroups;"), Code::UnsupportedExtension);
    assert_eq!(refused("enable nonsense;"), Code::UnsupportedExtension);
    assert_eq!(refused("requires buffer_view;"), Code::UnsupportedExtension);
    assert_eq!(
        refused("fn f() -> u32 { return subgroupAdd(1u); }"),
        Code::UnsupportedExtension
    );
    accepted("requires pointer_composite_access, swizzle_assignment;");
}

#[test]
fn pointers_never_alias_memory_a_callee_writes() {
    assert_eq!(
        refused(
            "fn f(p: ptr<function, i32>, q: ptr<function, i32>) { *p = *q; }
                 fn g() { var x = 1; f(&x, &x); }"
        ),
        Code::Pointer
    );
    accepted(
        "fn f(p: ptr<function, i32>, q: ptr<function, i32>) -> i32 { return *p + *q; }
              fn g() { var x = 1; _ = f(&x, &x); }",
    );
    assert_eq!(
        refused("var<private> x: i32; fn f(p: ptr<private, i32>) { *p = x; } fn g() { f(&x); }"),
        Code::Pointer,
        "written through the pointer, read directly"
    );
    assert_eq!(
        refused(
            "var<private> x: i32; fn f(p: ptr<private, i32>) -> i32 { x = 1; return *p; }
                 fn g() { _ = f(&x); }"
        ),
        Code::Pointer,
        "read through the pointer, written directly"
    );
    assert_eq!(
        refused(
            "fn h(p: ptr<function, i32>) { *p = 1; }
                 fn f(p: ptr<function, i32>, q: ptr<function, i32>) { h(p); _ = *q; }
                 fn g() { var x = 1; f(&x, &x); }"
        ),
        Code::Pointer,
        "a write two calls down"
    );
    accepted(
        "var<private> x: i32; var<private> y: i32;
              fn f(p: ptr<private, i32>) { *p = x; } fn g() { f(&y); }",
    );
}

const SAMPLING: &str = "
@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var s: sampler;
";

fn fragment(body: &str) -> alloc::string::String {
    alloc::format!(
        "{SAMPLING} @fragment fn fs(@location(0) uv: vec2f, @location(1) @interpolate(flat) n: u32) -> @location(0) vec4f {{ {body} }}"
    )
}

fn compute(body: &str) -> alloc::string::String {
    alloc::format!(
        "var<workgroup> shared_value: u32;
         @compute @workgroup_size(64)
         fn c(@builtin(local_invocation_index) i: u32, @builtin(workgroup_id) group: vec3u) {{ {body} }}"
    )
}

#[test]
fn a_derivative_stands_in_uniform_control_flow() {
    accepted(&fragment("return textureSample(t, s, uv);"));
    accepted(&fragment(
        "let c = textureSample(t, s, uv); if n == 0u { return c; } return vec4f();",
    ));
    assert_eq!(
        refused(&fragment(
            "if n == 0u { return textureSample(t, s, uv); } return vec4f();"
        )),
        Code::Uniformity
    );
    assert_eq!(
        refused(&fragment(
            "if uv.x > 0.5 && dpdx(uv.x) > 0.0 { return vec4f(); } return vec4f(1.0);"
        )),
        Code::Uniformity,
        "a short-circuit's right side runs where its left side decides"
    );
    assert_eq!(
        refused(&fragment(
            "if n == 0u { return vec4f(); } return textureSample(t, s, uv);"
        )),
        Code::Uniformity,
        "an early return leaves the rest non-uniform"
    );
}

#[test]
fn a_filter_relaxes_the_derivative_rule() {
    accepted(&fragment(
        "if n == 0u @diagnostic(off, derivative_uniformity) { return textureSample(t, s, uv); } return vec4f();",
    ));
    let warned = alloc::format!(
        "diagnostic(warning, derivative_uniformity); {}",
        fragment("if n == 0u { return textureSample(t, s, uv); } return vec4f();")
    );
    assert_eq!(check_with(&warned, &[]), Ok(()));
    assert_eq!(
        refused(&alloc::format!("diagnostic(off, derivative_uniformity); diagnostic(error, derivative_uniformity); {SAMPLING}")),
        Code::Directive,
        "two global filters give one rule two severities"
    );
    accepted(&alloc::format!(
        "diagnostic(off, no_such_rule); diagnostic(off, other.rule); {SAMPLING}"
    ));
}

#[test]
fn a_barrier_stands_in_uniform_control_flow() {
    accepted(&compute(
        "workgroupBarrier(); if group.x == 0u { workgroupBarrier(); }",
    ));
    assert_eq!(
        refused(&compute("if i == 0u { workgroupBarrier(); }")),
        Code::Uniformity
    );
    accepted(&compute(
        "for (var k = 0u; k < 4u; k++) { workgroupBarrier(); }",
    ));
    assert_eq!(
        refused(&compute(
            "for (var k = 0u; k < i; k++) { workgroupBarrier(); }"
        )),
        Code::Uniformity
    );
    assert_eq!(
        refused(&compute("if shared_value == 0u { workgroupBarrier(); }")),
        Code::Uniformity,
        "workgroup memory may differ between invocations"
    );
    accepted(&compute(
        "if workgroupUniformLoad(&shared_value) == 0u { workgroupBarrier(); }",
    ));
    assert_eq!(
        refused(&compute("if i == 0u { return; } workgroupBarrier();")),
        Code::Uniformity
    );
}

#[test]
fn uniformity_follows_variables_through_branches_and_loops() {
    assert_eq!(
        refused(&compute(
            "var x = 0u; if i == 0u { x = 1u; } if x == 0u { workgroupBarrier(); }"
        )),
        Code::Uniformity
    );
    accepted(&compute("var x = 0u; if group.x == 0u { x = 1u; } else { x = 2u; } if x == 0u { workgroupBarrier(); }"));
    assert_eq!(
        refused(&compute("var x = 0u; for (var k = 0u; k < 4u; k++) { if x == 0u { workgroupBarrier(); } x = i; }")),
        Code::Uniformity,
        "a value carried round the loop"
    );
    accepted(&compute(
        "var x = i; x = 3u; if x == 3u { workgroupBarrier(); }",
    ));
    assert_eq!(
        refused(&compute(
            "var a = array(0u, 1u); a[i % 2u] = 5u; if a[0] == 5u { workgroupBarrier(); }"
        )),
        Code::Uniformity,
        "a write at a non-uniform index taints the array"
    );
}

#[test]
fn uniformity_follows_calls() {
    let callee = "fn sync() { workgroupBarrier(); }
                  fn sync_if(x: u32) { if x == 0u { workgroupBarrier(); } }
                  fn same(x: u32) -> u32 { return x; }
                  fn bump(p: ptr<function, u32>, x: u32) { *p = x; }";
    let with = |body: &str| alloc::format!("{callee} {}", compute(body));
    accepted(&with("sync(); sync_if(1u); sync_if(group.x);"));
    assert_eq!(refused(&with("if i == 0u { sync(); }")), Code::Uniformity);
    assert_eq!(refused(&with("sync_if(i);")), Code::Uniformity);
    assert_eq!(
        refused(&with("if same(i) == 0u { workgroupBarrier(); }")),
        Code::Uniformity
    );
    accepted(&with("if same(group.y) == 0u { workgroupBarrier(); }"));
    assert_eq!(
        refused(&with(
            "var v = 0u; bump(&v, i); if v == 0u { workgroupBarrier(); }"
        )),
        Code::Uniformity,
        "a pointer argument's memory takes what the callee writes"
    );
    accepted(&with(
        "var v = 0u; bump(&v, 7u); if v == 7u { workgroupBarrier(); }",
    ));
}

#[test]
fn each_pointer_takes_what_its_callee_s_callee_feeds_it() {
    let callees = "fn both(p: ptr<function, u32>, q: ptr<function, u32>, x: u32) { *p = x; *q = x; }
                   fn relay(p: ptr<function, u32>, q: ptr<function, u32>, x: u32) { both(p, q, x); }";
    let with = |body: &str| alloc::format!("{callees} {}", compute(body));
    for read in ["a", "b"] {
        assert_eq!(
            refused(&with(&alloc::format!(
                "var a = 0u; var b = 0u; relay(&a, &b, i); if {read} == 0u {{ workgroupBarrier(); }}"
            ))),
            Code::Uniformity,
            "{read} takes the invocation's index"
        );
    }
    accepted(&with(
        "var a = 0u; var b = 0u; relay(&a, &b, group.x); if b == 0u { workgroupBarrier(); }",
    ));
}

#[test]
fn each_severity_reaches_what_a_call_feeds_a_pointer() {
    let source = "
        diagnostic(warning, derivative_uniformity);
        fn both(p: ptr<function, f32>, q: ptr<function, f32>, x: f32) { *p = x; *q = x; }
        @fragment fn main(@builtin(position) pos: vec4f) -> @location(0) vec4f {
            var a = 0.0;
            var b = 0.0;
            both(&a, &b, pos.x);
            if a > 0.5 { _ = dpdx(1.0); }
            if b > 0.5 @diagnostic(info, derivative_uniformity) { _ = dpdy(1.0); }
            return vec4f();
        }";
    let compiled = crate::compile(source, &Options::default()).expect("compiled");
    let reported: alloc::vec::Vec<_> = compiled
        .diagnostics
        .iter()
        .filter(|d| d.code == Code::Uniformity)
        .map(|d| d.severity)
        .collect();
    assert_eq!(
        reported,
        [crate::Severity::Warning, crate::Severity::Info],
        "each severity's first broken requirement is reported"
    );
}
