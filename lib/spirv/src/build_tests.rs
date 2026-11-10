//! The builder: what it writes is admitted, interned once, laid out in the
//! order SPIR-V requires, and refused where it is misused.

use crate::enumerant::{
    BuiltIn, Capability, Decoration, ExecutionMode, ExecutionModel, StorageClass,
};
use crate::{
    Access, BuildError, Builder, Function, GlslStd450, Id, Op, Rule, Stage, Version, WorkgroupSize,
};

/// What a compute shader over one storage buffer of words shares: the
/// buffer, each invocation's global id, and the types they need.
struct Words {
    uint: Id,
    word: Id,
    buffer: Id,
    global: Id,
    v3uint: Id,
}

fn words(b: &mut Builder) -> Result<Words, BuildError> {
    b.capability(Capability::SHADER)?;
    let uint = b.int(32, false)?;
    let v3uint = b.vector(uint, 3)?;
    let array = b.runtime_array(uint)?;
    b.decorate(array, Decoration::ARRAY_STRIDE, &[4])?;
    let block = b.structure(&[array])?;
    b.decorate(block, Decoration::BLOCK, &[])?;
    b.member_decorate(block, 0, Decoration::OFFSET, &[0])?;
    let block_ptr = b.pointer(StorageClass::STORAGE_BUFFER, block)?;
    let buffer = b.variable(block_ptr, StorageClass::STORAGE_BUFFER)?;
    b.decorate(buffer, Decoration::DESCRIPTOR_SET, &[0])?;
    b.decorate(buffer, Decoration::BINDING, &[0])?;
    let input = b.pointer(StorageClass::INPUT, v3uint)?;
    let global = b.variable(input, StorageClass::INPUT)?;
    b.decorate(
        global,
        Decoration::BUILT_IN,
        &[BuiltIn::GLOBAL_INVOCATION_ID.0],
    )?;
    let word = b.pointer(StorageClass::STORAGE_BUFFER, uint)?;
    Ok(Words {
        uint,
        word,
        buffer,
        global,
        v3uint,
    })
}

/// A compute module of 64-wide workgroups over [`words`] whose `main` is
/// `body`, given its invocation's index.
fn compute<B>(body: B) -> Result<crate::Admitted, BuildError>
where
    B: FnOnce(&mut Function<'_>, &Words, Id) -> Result<(), BuildError>,
{
    let mut b = Builder::new(Version::NEWEST, 0)?;
    let shared = words(&mut b)?;
    let (void, main) = (b.void()?, b.id()?);
    b.define(main, void, &[], |f, _| {
        let id = f.op(Op::LOAD, shared.v3uint, &[shared.global])?;
        let index = f.op_args(
            Op::COMPOSITE_EXTRACT,
            shared.uint,
            &[crate::Arg::Id(id), crate::Arg::Word(0)],
        )?;
        body(f, &shared, index)?;
        f.emit(Op::RETURN, &[])
    })?;
    b.entry_point(
        ExecutionModel::GL_COMPUTE,
        main,
        "main",
        &[shared.global, shared.buffer],
    )?;
    b.execution_mode(main, ExecutionMode::LOCAL_SIZE, &[64, 1, 1])?;
    b.finish()
}

/// Store `value` at word `index` of the buffer.
fn store(f: &mut Function<'_>, shared: &Words, index: Id, value: Id) -> Result<(), BuildError> {
    let zero = f.module().uint(0)?;
    let at = f.op(Op::ACCESS_CHAIN, shared.word, &[shared.buffer, zero, index])?;
    f.emit(Op::STORE, &[at, value])
}

#[test]
fn a_compute_module_is_admitted_with_what_its_entry_point_needs() {
    let admitted = compute(|f, shared, index| store(f, shared, index, index)).expect("admitted");
    let entry = admitted
        .entry_point("main", Stage::Compute)
        .expect("its entry point");
    assert_eq!(entry.workgroup(), Some(WorkgroupSize::Fixed([64, 1, 1])));
    let bindings = entry.bindings();
    assert_eq!(bindings.len(), 1);
    assert_eq!((bindings[0].set, bindings[0].binding), (0, 0));
    assert_eq!(bindings[0].access, Access::WRITE);
}

#[test]
fn a_type_or_constant_asked_for_twice_is_one_id_and_an_aggregate_is_fresh() {
    let mut b = Builder::new(Version::NEWEST, 0).expect("a builder");
    let uint = b.int(32, false).expect("a type");
    assert_eq!(b.int(32, false), Ok(uint));
    assert_ne!(b.int(32, true), Ok(uint));
    assert_eq!(b.uint(7), b.uint(7));
    assert_ne!(b.uint(7), b.uint(8));
    let v4 = b.vector(uint, 4).expect("a vector");
    assert_eq!(b.vector(uint, 4), Ok(v4));
    assert_eq!(
        b.pointer(StorageClass::FUNCTION, uint),
        b.pointer(StorageClass::FUNCTION, uint)
    );
    assert_ne!(b.runtime_array(uint), b.runtime_array(uint));
    assert_ne!(b.structure(&[uint]), b.structure(&[uint]));
    let void = b.void().expect("void");
    assert_eq!(
        b.function_type(void, &[uint]),
        b.function_type(void, &[uint])
    );
    assert_ne!(b.function_type(void, &[uint]), b.function_type(void, &[]));
    let float = b.float(32).expect("a float");
    assert_eq!(b.real(0.5), b.constant(float, 0.5f32.to_bits()));
    let double = b.float(64).expect("a double");
    assert_eq!(
        b.constant_wide(double, 1.0f64.to_bits()),
        b.constant_wide(double, 1.0f64.to_bits())
    );
}

#[test]
fn the_same_source_builds_the_same_bytes() {
    let build = || {
        compute(|f, shared, index| store(f, shared, index, index))
            .expect("admitted")
            .module()
            .bytes()
            .to_vec()
    };
    assert_eq!(build(), build());
}

#[test]
fn if_then_and_if_else_merge_and_go_on() {
    let admitted = compute(|f, shared, index| {
        let boolean = f.module().boolean()?;
        let limit = f.module().uint(32)?;
        let low = f.op(Op::U_LESS_THAN, boolean, &[index, limit])?;
        f.if_then(low, |f| store(f, shared, index, limit))?;
        f.if_else(
            low,
            |f| store(f, shared, index, index),
            |f| store(f, shared, index, limit),
        )?;
        // A branch that returns leaves nothing to merge from it.
        f.if_then(low, |f| f.emit(Op::RETURN, &[]))?;
        store(f, shared, index, index)
    });
    assert!(admitted.is_ok(), "{admitted:?}");
}

#[test]
fn a_while_loop_counts_through_a_local_laid_at_the_first_block() {
    let admitted = compute(|f, shared, index| {
        let (uint, boolean) = (shared.uint, f.module().boolean()?);
        let (zero, one, ten) = (
            f.module().uint(0)?,
            f.module().uint(1)?,
            f.module().uint(10)?,
        );
        let counter_ptr = f.module().pointer(StorageClass::FUNCTION, uint)?;
        // Asked for in the loop's first block, laid at the function's.
        let counter = f.local(counter_ptr)?;
        f.emit(Op::STORE, &[counter, zero])?;
        let total = f.local(counter_ptr)?;
        f.emit(Op::STORE, &[total, zero])?;
        f.while_loop(
            |f| {
                let at = f.op(Op::LOAD, uint, &[counter])?;
                f.op(Op::U_LESS_THAN, boolean, &[at, ten])
            },
            |f| {
                let (at, sum) = (
                    f.op(Op::LOAD, uint, &[counter])?,
                    f.op(Op::LOAD, uint, &[total])?,
                );
                let added = f.op(Op::I_ADD, uint, &[sum, at])?;
                f.emit(Op::STORE, &[total, added])?;
                // A nested construct inside the loop's body.
                let odd = f.op(Op::BITWISE_AND, uint, &[at, one])?;
                let is_odd = f.op(Op::I_NOT_EQUAL, boolean, &[odd, zero])?;
                f.if_then(is_odd, |f| store(f, shared, index, at))
            },
            |f| {
                let at = f.op(Op::LOAD, uint, &[counter])?;
                let next = f.op(Op::I_ADD, uint, &[at, one])?;
                f.emit(Op::STORE, &[counter, next])
            },
        )?;
        let sum = f.op(Op::LOAD, uint, &[total])?;
        store(f, shared, index, sum)
    });
    assert!(admitted.is_ok(), "{admitted:?}");
}

#[test]
fn a_glsl_instruction_imports_its_set_once() {
    let admitted = compute(|f, shared, index| {
        let one = f.module().uint(1)?;
        let high = f.glsl(GlslStd450::U_MAX, shared.uint, &[index, one])?;
        let low = f.glsl(GlslStd450::U_MIN, shared.uint, &[high, index])?;
        store(f, shared, index, low)
    })
    .expect("admitted");
    let imports = admitted
        .module()
        .instructions()
        .iter()
        .filter(|inst| inst.op() == Op::EXT_INST_IMPORT)
        .count();
    assert_eq!(imports, 1);
}

#[test]
fn a_function_called_before_it_is_defined_is_admitted() {
    let mut b = Builder::new(Version::NEWEST, 0).expect("a builder");
    let shared = words(&mut b).expect("shared");
    let (void, main, twice) = (b.void().unwrap(), b.id().unwrap(), b.id().unwrap());
    b.define(main, void, &[], |f, _| {
        let id = f.op(Op::LOAD, shared.v3uint, &[shared.global])?;
        let index = f.op_args(
            Op::COMPOSITE_EXTRACT,
            shared.uint,
            &[crate::Arg::Id(id), crate::Arg::Word(0)],
        )?;
        let doubled = f.op(Op::FUNCTION_CALL, shared.uint, &[twice, index])?;
        store(f, &shared, index, doubled)?;
        f.emit(Op::RETURN, &[])
    })
    .expect("main");
    b.define(twice, shared.uint, &[shared.uint], |f, params| {
        let sum = f.op(Op::I_ADD, shared.uint, &[params[0], params[0]])?;
        f.emit(Op::RETURN_VALUE, &[sum])
    })
    .expect("twice");
    b.entry_point(
        ExecutionModel::GL_COMPUTE,
        main,
        "main",
        &[shared.global, shared.buffer],
    )
    .unwrap();
    b.execution_mode(main, ExecutionMode::LOCAL_SIZE, &[64, 1, 1])
        .unwrap();
    assert!(b.finish().is_ok());
}

#[test]
fn an_instruction_after_a_terminator_has_no_block() {
    let built = compute(|f, shared, index| {
        f.emit(Op::RETURN, &[])?;
        store(f, shared, index, index)
    });
    assert_eq!(built.err(), Some(BuildError::NoBlock));
}

#[test]
fn a_block_begun_while_one_is_open_or_a_function_left_open_is_unterminated() {
    let built = compute(|f, _, _| {
        let next = f.label()?;
        f.begin(next)
    });
    assert_eq!(built.err(), Some(BuildError::Unterminated));

    let mut b = Builder::new(Version::NEWEST, 0).expect("a builder");
    let (void, main) = (b.void().unwrap(), b.id().unwrap());
    let left = b.define(main, void, &[], |_, _| Ok(()));
    assert_eq!(left, Err(BuildError::Unterminated));
}

#[test]
fn a_module_the_validator_refuses_is_refused_not_built() {
    // A capability off the allow-list.
    let mut b = Builder::new(Version::NEWEST, 0).expect("a builder");
    b.capability(Capability::KERNEL).unwrap();
    assert!(matches!(b.finish(), Err(BuildError::Refused(_))));

    // An entry point whose interface leaves out a variable it reads.
    let mut b = Builder::new(Version::NEWEST, 0).expect("a builder");
    let shared = words(&mut b).unwrap();
    let (void, main) = (b.void().unwrap(), b.id().unwrap());
    b.define(main, void, &[], |f, _| {
        let id = f.op(Op::LOAD, shared.v3uint, &[shared.global])?;
        let index = f.op_args(
            Op::COMPOSITE_EXTRACT,
            shared.uint,
            &[crate::Arg::Id(id), crate::Arg::Word(0)],
        )?;
        store(f, &shared, index, index)?;
        f.emit(Op::RETURN, &[])
    })
    .unwrap();
    b.entry_point(ExecutionModel::GL_COMPUTE, main, "main", &[shared.buffer])
        .unwrap();
    b.execution_mode(main, ExecutionMode::LOCAL_SIZE, &[64, 1, 1])
        .unwrap();
    let refused = b.finish();
    assert!(
        matches!(refused, Err(BuildError::Refused(ref refusal)) if refusal.rule == Rule::Interface),
        "{refused:?}"
    );
}
