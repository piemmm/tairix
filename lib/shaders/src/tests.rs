//! Every committed module is admitted and needs exactly the push constants
//! and bindings its pass states.

use alloc::vec::Vec;

use tairix_spirv::{Access, BindingKind, Module, Stage, WorkgroupSize};

use crate::{chrome, compose, frost, indirect, paint, GROUP, SET};

const READ: Access = Access::READ;
const WRITE: Access = Access::WRITE;
const BOTH: Access = Access::READ.with(Access::WRITE);

/// A module's name, its workgroup, its push words, and each binding it
/// reads, writes or both.
type Stated = (&'static str, [u32; 3], usize, &'static [(u32, Access)]);

/// A pass over a picture's pixels or cells.
const GRID: [u32; 3] = [GROUP, GROUP, 1];

/// A check of a command's few words.
const ONCE: [u32; 3] = [1, 1, 1];

/// What each module's pass states.
const STATED: [Stated; 19] = [
    (
        "frost_reduce",
        GRID,
        frost::reduce::WORDS,
        &[(frost::reduce::PIXELS, READ), (frost::reduce::CELLS, WRITE)],
    ),
    (
        "frost_blur",
        GRID,
        frost::blur::WORDS,
        &[(frost::blur::SOURCE, READ), (frost::blur::TARGET, WRITE)],
    ),
    (
        "frost_spread",
        GRID,
        frost::spread::WORDS,
        &[(frost::spread::CELLS, READ), (frost::spread::PIXELS, WRITE)],
    ),
    (
        "frost_mix",
        GRID,
        frost::mix::WORDS,
        &[
            (frost::mix::PIXELS, BOTH),
            (frost::mix::BLUR, READ),
            (frost::mix::COVERAGE, READ),
        ],
    ),
    (
        "over",
        GRID,
        compose::over::WORDS,
        &[(compose::over::TARGET, BOTH), (compose::over::SOURCE, READ)],
    ),
    (
        "copy",
        GRID,
        compose::copy::WORDS,
        &[
            (compose::copy::TARGET, WRITE),
            (compose::copy::SOURCE, READ),
        ],
    ),
    (
        "fill",
        GRID,
        compose::fill::WORDS,
        &[(compose::fill::TARGET, WRITE)],
    ),
    (
        "encode",
        GRID,
        compose::encode::WORDS,
        &[
            (compose::encode::TARGET, WRITE),
            (compose::encode::SOURCE, READ),
        ],
    ),
    (
        "resample",
        GRID,
        compose::resample::WORDS,
        &[
            (compose::resample::TARGET, WRITE),
            (compose::resample::SOURCE, READ),
            (compose::resample::PLAN, READ),
        ],
    ),
    (
        "body",
        GRID,
        chrome::WORDS,
        &[
            (chrome::TARGET, BOTH),
            (chrome::body::CLIENT, READ),
            (chrome::body::FURNITURE, READ),
            (chrome::body::LAYOUT, READ),
        ],
    ),
    (
        "shadow",
        GRID,
        chrome::shadow::WORDS,
        &[(chrome::TARGET, BOTH), (chrome::shadow::TABLES, READ)],
    ),
    ("plate", GRID, paint::plate::WORDS, &[(paint::TARGET, BOTH)]),
    (
        "wash",
        GRID,
        paint::wash::WORDS,
        &[(paint::TARGET, BOTH), (paint::wash::PLANES, READ)],
    ),
    ("ring", GRID, paint::ring::WORDS, &[(paint::TARGET, BOTH)]),
    (
        "coverage",
        GRID,
        paint::coverage::WORDS,
        &[
            (paint::coverage::PLANE, WRITE),
            (paint::coverage::PIECES, READ),
        ],
    ),
    (
        "mask",
        GRID,
        paint::mask::WORDS,
        &[(paint::TARGET, BOTH), (paint::mask::PLANE, READ)],
    ),
    (
        "picture",
        GRID,
        paint::picture::WORDS,
        &[(paint::TARGET, BOTH), (paint::picture::SOURCE, READ)],
    ),
    (
        "indirect_counts",
        ONCE,
        indirect::counts::WORDS,
        &[
            (indirect::counts::COUNTS, BOTH),
            (indirect::counts::FAULTS, BOTH),
        ],
    ),
    (
        "indirect_records",
        ONCE,
        indirect::records::WORDS,
        &[
            (indirect::records::RECORDS, BOTH),
            (indirect::records::FAULTS, BOTH),
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
        let (_, workgroup, words, bindings) = STATED
            .iter()
            .find(|(name, _, _, _)| *name == pinned.name)
            .unwrap_or_else(|| panic!("{} states nothing", pinned.name));
        assert_eq!(
            entry.workgroup(),
            Some(WorkgroupSize::Fixed(*workgroup)),
            "{}",
            pinned.name
        );
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

/// A description is refused just past each bound that keeps its pass in
/// 32-bit words, and taken at the bound.
#[test]
fn a_description_past_a_bound_is_refused() {
    use tairix_raster::{Caster, Color, DropShadow, RoundRect, MAX_DRAWING_EXTENT};

    use crate::chrome::body::{Layout, Strip};
    use crate::chrome::shadow::Tables;
    use crate::chrome::Push;
    use crate::{Refused, MAX_PASS_COORDINATE, MAX_PASS_RADIUS};

    let strip = |width, height, stride| Strip {
        width,
        height,
        stride,
        ..Strip::default()
    };
    let rounded = |radius| RoundRect {
        width: 8,
        height: 8,
        radius,
    };
    let layout = |client, cut| Layout {
        origin: (0, 0),
        opacity: 255,
        client,
        drawn: (4, 4),
        backdrop: None,
        cut,
        furniture: [Strip::default(); 4],
    };
    let cut = |radius| Some((rounded(radius), (0, 0)));
    assert!(layout(strip(4, 4, 4), cut(MAX_PASS_RADIUS)).words().is_ok());
    assert_eq!(
        layout(strip(4, 4, 4), cut(MAX_PASS_RADIUS + 1)).words(),
        Err(Refused::Radius)
    );
    let widest = MAX_DRAWING_EXTENT;
    assert!(layout(strip(widest, 1, widest), None).words().is_ok());
    assert_eq!(
        layout(strip(widest + 1, 1, widest + 1), None).words(),
        Err(Refused::Extent)
    );
    assert_eq!(
        layout(strip(widest, widest, widest), None).words(),
        Err(Refused::Extent),
        "a client more pixels than a surface holds"
    );

    let dark = DropShadow::new(6, Color::rgba(0, 0, 0, 0x8c));
    let tables = Tables::new(&dark, &dark).expect("tables");
    let frame = Push {
        stride: 8,
        left: 0,
        top: 0,
        width: 8,
        height: 8,
        dithered: true,
    };
    let describe = |caster: &Caster| tables.push(&frame, caster, 128);
    let caster = |left: i32, radius| Caster {
        left,
        top: 0,
        right: left + 10,
        bottom: 10,
        silhouette: Some(RoundRect {
            width: 10,
            height: 10,
            radius,
        }),
        opacity: 255,
    };
    let at_bound = MAX_PASS_COORDINATE - 10;
    for left in [0, at_bound, -at_bound] {
        assert!(describe(&caster(left, 4)).is_ok(), "{left}");
    }
    for left in [MAX_PASS_COORDINATE + 1, -MAX_PASS_COORDINATE - 1] {
        assert_eq!(
            describe(&caster(left, 4)),
            Err(Refused::Coordinate),
            "{left}"
        );
    }
    assert_eq!(
        describe(&caster(0, MAX_PASS_RADIUS + 1)),
        Err(Refused::Radius)
    );
}
