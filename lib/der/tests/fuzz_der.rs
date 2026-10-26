//! Deterministic fuzz harness for the DER reader and writer.
//!
//! The invariants:
//!
//! * reading any bytes — as a tree of elements to [`MAX_DEPTH`], and through
//!   every typed reader — never panics; it yields a value or a `DerError`;
//! * DER has one encoding per value, so a tree the reader accepts re-encodes
//!   to exactly the bytes it was read from, and so does every typed value.
//!
//! A per-run-seeded `Prng` draws noise, mutates encodings the writer built,
//! and splices them. A plain `cargo test` runs the [`SMOKE_ITERATIONS`] sweep
//! once from a fresh, logged seed; `cargo xtask fuzz` exports
//! `TAIRIX_FUZZ_BUDGET_SECS` to extend it to a wall-clock budget.

use tairix_abi::time::Time64;
use tairix_der::{Class, DerError, Reader, Tag, Writer, MAX_DEPTH};
use tairix_fuzzseed::Prng;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 20_000;

/// Largest noise input.
const MAX_NOISE: usize = 256;

/// A parsed element tree, holding just enough to re-encode it.
enum Node {
    Primitive(Tag, Vec<u8>),
    Constructed(Tag, Vec<Node>),
}

/// Read every element of `reader` as a tree, descending into each
/// constructed one.
fn tree(reader: &mut Reader<'_>, depth: usize) -> Result<Vec<Node>, DerError> {
    let mut nodes = Vec::new();
    while !reader.is_empty() {
        let tlv = reader.tlv()?;
        if tlv.tag.is_constructed() {
            if depth >= MAX_DEPTH {
                return Err(DerError::TooDeep);
            }
            let mut inner = Reader::new(tlv.contents);
            nodes.push(Node::Constructed(tlv.tag, tree(&mut inner, depth + 1)?));
        } else {
            typed(tlv.tag, tlv.encoding);
            nodes.push(Node::Primitive(tlv.tag, tlv.contents.to_vec()));
        }
    }
    Ok(nodes)
}

fn encode(writer: &mut Writer, nodes: &[Node]) {
    for node in nodes {
        match node {
            Node::Primitive(tag, contents) => writer.element(*tag, contents),
            Node::Constructed(tag, children) => writer.constructed(*tag, |w| encode(w, children)),
        }
    }
}

/// Read a universal primitive through its typed reader and, when it is
/// accepted, check it re-encodes to the same bytes.
fn typed(tag: Tag, encoding: &[u8]) {
    let reread = |write: &dyn Fn(&mut Writer)| {
        let mut writer = Writer::new();
        write(&mut writer);
        assert_eq!(writer.finish().expect("encodes"), encoding, "{tag:?}");
    };
    let mut reader = Reader::new(encoding);
    match tag {
        Tag::INTEGER => {
            if let Ok(value) = reader.integer() {
                reread(&|w| w.integer(value));
            }
        }
        Tag::ENUMERATED => {
            if let Ok(value) = reader.enumerated() {
                reread(&|w| w.enumerated(value));
            }
        }
        Tag::BOOLEAN => {
            if let Ok(value) = reader.boolean() {
                reread(&|w| w.boolean(value));
            }
        }
        Tag::OBJECT_IDENTIFIER => {
            if let Ok(arcs) = reader.oid() {
                reread(&|w| w.oid(arcs));
            }
        }
        Tag::BIT_STRING => {
            if let Ok(bits) = reader.bit_string() {
                reread(&|w| w.bit_string(bits.unused_bits(), bits.bytes()));
            }
        }
        Tag::GENERALIZED_TIME => {
            if let Ok(time) = reader.generalized_time() {
                reread(&|w| w.generalized_time(time));
            }
        }
        _ => {}
    }
}

/// A valid encoding the writer builds from `rng`, for the mutator to break.
fn template(rng: &mut Prng, depth: usize) -> Vec<u8> {
    let mut writer = Writer::new();
    build(rng, &mut writer, depth);
    writer
        .finish()
        .expect("the writer builds what it validates")
}

fn build(rng: &mut Prng, writer: &mut Writer, depth: usize) {
    match rng.below(if depth == 0 { 8 } else { 10 }) {
        0 => writer.integer(i64::from(rng.next_u32()) - i64::from(rng.next_u32())),
        1 => writer.boolean(rng.below(2) == 0),
        2 => {
            let mut bytes = vec![0; rng.at_most(40)];
            rng.fill(&mut bytes);
            writer.octet_string(&bytes);
        }
        3 => writer.oid(rng.pick(&[
            &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x02][..],
            &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x12, 0x01, 0x02, 0x02],
        ])),
        4 => writer.generalized_time(Time64::from_secs(i64::from(rng.next_u32()))),
        5 => writer.bit_string(0, &rng.next_u32().to_be_bytes()),
        6 => writer.general_string(b"EXAMPLE.ORG"),
        7 => writer.null(),
        8 => writer.sequence(|w| {
            for _ in 0..rng.at_most(4) {
                build(rng, w, depth - 1);
            }
        }),
        _ => {
            let number = u32::try_from(rng.below(40)).unwrap_or(0);
            let class = *rng.pick(&[Class::Context, Class::Application]);
            writer.constructed(Tag::new(class, true, number), |w| build(rng, w, depth - 1));
        }
    }
}

/// One round: read `input` whole, and if it is accepted, re-encode it.
fn exercise(input: &[u8]) {
    let mut reader = Reader::new(input);
    if let Ok(nodes) = tree(&mut reader, 0) {
        let mut writer = Writer::new();
        encode(&mut writer, &nodes);
        assert_eq!(writer.finish().expect("re-encodes"), input);
    }
}

#[test]
fn reading_never_panics_and_accepted_der_round_trips() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "reading_never_panics_and_accepted_der_round_trips",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let mut noise = vec![0; rng.at_most(MAX_NOISE)];
        rng.fill(&mut noise);
        exercise(&noise);

        let valid = template(&mut rng, 4);
        exercise(&valid);

        let mut mutated = valid.clone();
        if !mutated.is_empty() {
            for _ in 0..=rng.below(3) {
                let at = rng.below(mutated.len());
                mutated[at] = rng.edge_biased_u8();
            }
        }
        exercise(&mutated);

        let mut spliced = template(&mut rng, 2);
        let cut = rng.at_most(valid.len());
        spliced.extend_from_slice(&valid[..cut]);
        exercise(&spliced);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
