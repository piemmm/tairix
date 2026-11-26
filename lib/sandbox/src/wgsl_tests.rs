use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_spirv::{Stage, WorkgroupSize};
use tairix_wgsl::{Code, Options, OverrideKey, Severity, MAX_SOURCE_BYTES};

use super::{compile, CompileFailure, Fault, WgslService, MAX_OVERRIDES, MAX_OVERRIDE_NAME_BYTES};
use crate::testing::{loopback, scripted};
use crate::wire::Writer;
use crate::worker::Service;

const KERNEL: &str = "
@group(0) @binding(0) var<storage, read_write> out: array<u32>;
override width: u32 = 4;
@id(9) override height: u32 = 1;
@compute @workgroup_size(width, height)
fn main(@builtin(global_invocation_id) id: vec3u) {
    out[id.x] = id.x * 3u;
}
";

/// A fragment shader whose filter lowers a derivative's uniformity error to
/// a warning, on its fourth line.
const WARNED: &str = "diagnostic(warning, derivative_uniformity);
@fragment
fn main(@location(0) x: f32) -> @location(0) vec4f {
    var d = 0.0; if x > 0.5 { d = dpdx(x); }
    return vec4f(d);
}
";

/// The bytes `source` compiles to here, for a scripted worker to answer.
fn module_of(source: &str) -> Vec<u8> {
    tairix_wgsl::compile(source, &Options::default())
        .expect("compiles")
        .module
}

/// A reply answering `module` with the diagnostics `write` writes.
fn module_reply(module: &[u8], count: u16, write: impl FnOnce(&mut Writer)) -> Vec<u8> {
    let mut w = Writer::new();
    w.u8(super::REPLY_MODULE);
    w.bytes(module);
    w.u16(count);
    write(&mut w);
    w.finish()
}

/// One diagnostic's wire form.
fn diagnostic(w: &mut Writer, (severity, code, start, end): (u8, u16, u32, u32), message: &str) {
    w.u8(severity);
    w.u16(code);
    w.u32(start);
    w.u32(end);
    w.str(message);
}

#[test]
fn a_kernel_is_compiled_in_the_worker_and_admitted_here() {
    let mut sandbox = loopback::<WgslService>();
    let compiled = compile(&mut sandbox, KERNEL, &[]).expect("compiled");
    let entry = compiled
        .module
        .entry_point("main", Stage::Compute)
        .expect("its entry point");
    assert_eq!(entry.workgroup(), Some(WorkgroupSize::Fixed([4, 1, 1])));
    assert!(compiled.diagnostics.is_empty());
    assert!(
        sandbox.is_live(),
        "an honest worker is kept for the next compile"
    );
}

#[test]
fn overrides_by_name_and_by_id_reach_the_compile() {
    let mut sandbox = loopback::<WgslService>();
    let overrides = [(OverrideKey::Name("width"), 8.0), (OverrideKey::Id(9), 2.0)];
    let compiled = compile(&mut sandbox, KERNEL, &overrides).expect("compiled");
    let entry = compiled
        .module
        .entry_point("main", Stage::Compute)
        .expect("main");
    assert_eq!(entry.workgroup(), Some(WorkgroupSize::Fixed([8, 2, 1])));
    let refused = compile(&mut sandbox, KERNEL, &[(OverrideKey::Name("depth"), 1.0)]);
    assert!(matches!(refused, Err(CompileFailure::Source(d)) if d.code == Code::OverrideValue));
}

#[test]
fn a_warning_is_placed_in_this_sides_source() {
    let mut sandbox = loopback::<WgslService>();
    let compiled = compile(&mut sandbox, WARNED, &[]).expect("compiled");
    let [warning] = compiled.diagnostics.as_slice() else {
        panic!("{:?}", compiled.diagnostics);
    };
    assert_eq!(warning.severity, Severity::Warning);
    assert_eq!(warning.code, Code::Uniformity);
    assert_eq!(warning.line, 4);
    assert_eq!(
        (warning.line, warning.column),
        tairix_wgsl::position(WARNED, warning.span.start)
    );
}

#[test]
fn a_refused_source_answers_its_error_at_its_place() {
    let mut sandbox = loopback::<WgslService>();
    let source = "@compute @workgroup_size(1)\nfn main() {\n    let x: u32 = 1.5;\n}\n";
    match compile(&mut sandbox, source, &[]) {
        Err(CompileFailure::Source(error)) => {
            assert_eq!(error.severity, Severity::Error);
            assert_eq!((error.line, error.column), (3, 18));
        }
        other => panic!("{other:?}"),
    }
    assert!(
        sandbox.is_live(),
        "a source error is the worker's to answer"
    );
}

#[test]
fn a_request_past_its_bounds_is_refused_here_unsent() {
    let mut sandbox = loopback::<WgslService>();
    let long = " ".repeat(MAX_SOURCE_BYTES + 1);
    assert_eq!(
        compile(&mut sandbox, &long, &[]).err(),
        Some(CompileFailure::TooLarge)
    );
    let many: Vec<(OverrideKey<'_>, f64)> = (0..=MAX_OVERRIDES)
        .map(|_| (OverrideKey::Id(1), 0.0))
        .collect();
    assert_eq!(
        compile(&mut sandbox, KERNEL, &many).err(),
        Some(CompileFailure::TooLarge)
    );
    let name: String = "n".repeat(MAX_OVERRIDE_NAME_BYTES + 1);
    let named = [(OverrideKey::Name(&name), 0.0)];
    assert_eq!(
        compile(&mut sandbox, KERNEL, &named).err(),
        Some(CompileFailure::TooLarge)
    );
    assert!(!sandbox.is_live(), "no worker was started for any of them");
}

#[test]
fn a_reply_that_cannot_be_believed_retires_its_worker() {
    let module = module_of(KERNEL);
    let source_error = |severity, start, end, message: &str| {
        let mut w = Writer::new();
        w.u8(super::REPLY_SOURCE);
        diagnostic(&mut w, (severity, Code::Type.number(), start, end), message);
        w.finish()
    };
    let lies: Vec<(&str, Vec<u8>)> = vec![
        (
            "a module the validator refuses",
            module_reply(&[3, 2, 35, 7, 0, 0, 1, 0], 0, |_| {}),
        ),
        (
            "a module cut short",
            module_reply(&module[..module.len() - 4], 0, |_| {}),
        ),
        (
            "an error among a module's diagnostics",
            module_reply(&module, 1, |w| {
                diagnostic(w, (2, Code::Type.number(), 0, 1), "error");
            }),
        ),
        (
            "a diagnostic past the source",
            module_reply(&module, 1, |w| {
                diagnostic(w, (1, Code::Uniformity.number(), 0, 100_000), "past");
            }),
        ),
        (
            "a code the front end has not",
            module_reply(&module, 1, |w| diagnostic(w, (1, 999, 0, 1), "unknown")),
        ),
        (
            "a message holding a control character",
            module_reply(&module, 1, |w| {
                diagnostic(w, (1, Code::Uniformity.number(), 0, 1), "a\u{1b}[2J");
            }),
        ),
        (
            "a source error of a warning's severity",
            source_error(1, 0, 1, "w"),
        ),
        (
            "a source error past the source",
            source_error(2, 5, 1_000_000, "e"),
        ),
        ("a reply of no known kind", vec![9]),
        ("a fault of no known kind", vec![super::REPLY_FAULT, 9]),
        (
            "bytes after the answer",
            [module_reply(&module, 0, |_| {}), vec![0]].concat(),
        ),
        ("an empty reply", Vec::new()),
    ];
    for (lie, reply) in lies {
        let mut sandbox = scripted(reply);
        assert_eq!(
            compile(&mut sandbox, KERNEL, &[]).err(),
            Some(CompileFailure::ReplyMalformed),
            "{lie}"
        );
        assert!(!sandbox.is_live(), "{lie}: the worker is retired");
    }
}

#[test]
fn a_diagnostic_inside_a_character_cannot_be_believed() {
    let source = "// é\n@compute @workgroup_size(1) fn main() {}\n";
    let module = module_of(source);
    let inside = module_reply(&module, 1, |w| {
        diagnostic(w, (1, Code::Uniformity.number(), 4, 5), "x");
    });
    let mut sandbox = scripted(inside);
    assert_eq!(
        compile(&mut sandbox, source, &[]).err(),
        Some(CompileFailure::ReplyMalformed)
    );
}

#[test]
fn a_worker_calling_a_well_formed_request_malformed_is_retired() {
    let mut sandbox = scripted(super::fault_reply(Fault::MalformedRequest));
    assert_eq!(
        compile(&mut sandbox, KERNEL, &[]).err(),
        Some(CompileFailure::Fault(Fault::MalformedRequest))
    );
    assert!(!sandbox.is_live());
    for fault in [Fault::OutOfMemory, Fault::Internal] {
        let mut sandbox = scripted(super::fault_reply(fault));
        assert_eq!(
            compile(&mut sandbox, KERNEL, &[]).err(),
            Some(CompileFailure::Fault(fault))
        );
        assert!(sandbox.is_live(), "{fault:?} is the front end's to report");
    }
}

#[test]
fn the_worker_refuses_a_malformed_request() {
    let well_formed = {
        let mut w = Writer::new();
        w.u8(super::OP_COMPILE);
        w.u16(1);
        w.u8(super::KEY_ID);
        w.u16(9);
        w.u64(2.0f64.to_bits());
        w.str(KERNEL);
        w.finish()
    };
    let mut service = WgslService;
    assert_eq!(
        service.handle(&well_formed).first(),
        Some(&super::REPLY_MODULE)
    );
    let mut wrong_op = well_formed.clone();
    wrong_op[0] = 7;
    let mut wrong_key = well_formed.clone();
    wrong_key[3] = 5;
    let mut too_many = well_formed.clone();
    too_many[1..3].copy_from_slice(&u16::MAX.to_le_bytes());
    let malformed = [
        Vec::new(),
        wrong_op,
        wrong_key,
        too_many,
        well_formed[..well_formed.len() - 1].to_vec(),
        [well_formed.clone(), vec![0]].concat(),
    ];
    for request in malformed {
        assert_eq!(
            service.handle(&request),
            super::fault_reply(Fault::MalformedRequest),
            "{request:?}"
        );
    }
}
