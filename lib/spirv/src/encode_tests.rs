use crate::{Arg, EncodeError, Encoder, Id, Module, Op, Version};

const V1_0: Version = Version { major: 1, minor: 0 };

#[test]
fn ids_are_issued_densely_and_the_bound_follows_them() {
    let mut encoder = Encoder::new(V1_0, 7).expect("a header");
    let first = encoder.next_id().expect("an id");
    let second = encoder.next_id().expect("an id");
    assert_eq!((first.get(), second.get()), (1, 2));
    encoder
        .emit(Op::TYPE_VOID, &[Arg::Id(first)])
        .expect("written");
    let module = Module::decode(&encoder.finish()).expect("a valid module");
    assert_eq!(module.bound(), 3);
    assert_eq!(module.generator(), 7);
}

#[test]
fn an_id_never_issued_is_refused_and_nothing_is_written() {
    let mut encoder = Encoder::new(V1_0, 0).expect("a header");
    let stray = Id::new(5).expect("nonzero");
    assert_eq!(
        encoder.emit(Op::TYPE_VOID, &[Arg::Id(stray)]),
        Err(EncodeError::IdNotIssued)
    );
    assert_eq!(encoder.finish().len(), 20, "the header alone");
}

#[test]
fn a_string_is_terminated_and_padded_to_whole_words() {
    for (text, words) in [("", 1), ("abc", 1), ("main", 2), ("GLSL.std.450", 4)] {
        let mut encoder = Encoder::new(V1_0, 0).expect("a header");
        encoder
            .emit(Op::SOURCE_EXTENSION, &[Arg::Text(text)])
            .expect("written");
        let bytes = encoder.finish();
        assert_eq!(bytes.len(), 20 + 4 + words * 4, "{text:?}");
        let module = Module::decode(&bytes).expect("a valid module");
        let instruction = module.instructions()[0];
        assert_eq!(
            module.value(&module.operands(&instruction)[0]),
            crate::Value::Text(text)
        );
    }
    let mut encoder = Encoder::new(V1_0, 0).expect("a header");
    assert_eq!(
        encoder.emit(Op::SOURCE_EXTENSION, &[Arg::Text("a\0b")]),
        Err(EncodeError::InteriorNul)
    );
}

#[test]
fn an_instruction_past_its_word_count_is_refused() {
    let mut encoder = Encoder::new(V1_0, 0).expect("a header");
    let long = "x".repeat(0xffff * 4);
    assert_eq!(
        encoder.emit(Op::SOURCE_EXTENSION, &[Arg::Text(&long)]),
        Err(EncodeError::TooLong)
    );
}

#[test]
fn a_copy_counts_every_id_of_its_module_as_issued() {
    let mut source = Encoder::new(V1_0, 0).expect("a header");
    let ids: [Id; 3] = core::array::from_fn(|_| source.next_id().expect("an id"));
    source
        .emit(Op::TYPE_VOID, &[Arg::Id(ids[2])])
        .expect("written");
    let module = Module::decode(&source.finish()).expect("a valid module");
    let mut encoder = Encoder::new(V1_0, 0).expect("a header");
    encoder
        .copy(&module, &module.instructions()[0])
        .expect("copied");
    assert_eq!(encoder.next_id().map(Id::get), Ok(module.bound()));
}
