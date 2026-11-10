//! Every committed module is admitted and needs exactly the push constants
//! and bindings its pass states.

use alloc::vec::Vec;

use tairix_spirv::{Access, BindingKind, Module, Stage, WorkgroupSize};

use crate::{compose, frost, GROUP, SET};

const READ: Access = Access::READ;
const WRITE: Access = Access::WRITE;
const BOTH: Access = Access::READ.with(Access::WRITE);

/// A module's name, its push words, and each binding it reads, writes or
/// both.
type Stated = (&'static str, usize, &'static [(u32, Access)]);

/// What each module's pass states.
const STATED: [Stated; 8] = [
    (
        "frost_reduce",
        frost::reduce::WORDS,
        &[(frost::reduce::PIXELS, READ), (frost::reduce::CELLS, WRITE)],
    ),
    (
        "frost_blur",
        frost::blur::WORDS,
        &[(frost::blur::SOURCE, READ), (frost::blur::TARGET, WRITE)],
    ),
    (
        "frost_spread",
        frost::spread::WORDS,
        &[(frost::spread::CELLS, READ), (frost::spread::PIXELS, WRITE)],
    ),
    (
        "frost_mix",
        frost::mix::WORDS,
        &[
            (frost::mix::PIXELS, BOTH),
            (frost::mix::BLUR, READ),
            (frost::mix::COVERAGE, READ),
        ],
    ),
    (
        "over",
        compose::over::WORDS,
        &[(compose::over::TARGET, BOTH), (compose::over::SOURCE, READ)],
    ),
    (
        "copy",
        compose::copy::WORDS,
        &[
            (compose::copy::TARGET, WRITE),
            (compose::copy::SOURCE, READ),
        ],
    ),
    (
        "fill",
        compose::fill::WORDS,
        &[(compose::fill::TARGET, WRITE)],
    ),
    (
        "encode",
        compose::encode::WORDS,
        &[
            (compose::encode::TARGET, WRITE),
            (compose::encode::SOURCE, READ),
        ],
    ),
];

#[test]
fn every_committed_module_needs_what_its_pass_states() {
    for pinned in SET {
        let module = Module::decode(pinned.spirv).expect("decoded");
        let admitted = tairix_spirv::Admitted::new(module).expect("admitted");
        let entry = admitted
            .entry_point("main", Stage::Compute)
            .unwrap_or_else(|| panic!("{} has no compute main", pinned.name));
        assert_eq!(
            entry.workgroup(),
            Some(WorkgroupSize::Fixed([GROUP, GROUP, 1])),
            "{}",
            pinned.name
        );
        let (_, words, bindings) = STATED
            .iter()
            .find(|(name, _, _)| *name == pinned.name)
            .unwrap_or_else(|| panic!("{} states nothing", pinned.name));
        assert_eq!(
            entry.push_constants(),
            Some(u32::try_from(words * 4).expect("small")),
            "{}",
            pinned.name
        );
        let mut found: Vec<(u32, Access)> = entry
            .bindings()
            .iter()
            .map(|binding| {
                assert_eq!(
                    (binding.set, binding.kind, binding.count),
                    (0, BindingKind::StorageBuffer, 1)
                );
                (binding.binding, binding.access)
            })
            .collect();
        found.sort_by_key(|(binding, _)| *binding);
        let mut expected = bindings.to_vec();
        expected.sort_by_key(|(binding, _)| *binding);
        assert_eq!(found, expected, "{}", pinned.name);
    }
}
