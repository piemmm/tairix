use tairix_log::{EventId, Field, Level};

use super::*;

const KEYBOARD: u32 = 0x8007_0008;
const MOUSE: u32 = 0x8007_0028;

fn event<'a>(audit: AuditEvent, fields: &'a [Field<'a>]) -> Event<'a> {
    Event {
        level: Level::Info,
        id: EventId(audit.id().0),
        message: "",
        fields,
    }
}

const SECOND: Tables = Tables::Walked(Stage::Second);
const FIRST: Tables = Tables::Walked(Stage::First);

fn unit(outcome: &'static str, stopped: u64) -> [Field<'static>; 4] {
    record(outcome, SECOND, stopped, 0)
}

fn record(
    outcome: &'static str,
    tables: Tables,
    stopped: u64,
    refused: u64,
) -> [Field<'static>; 4] {
    [
        Field {
            key: "outcome",
            value: FieldValue::Str(outcome),
        },
        Field {
            key: "stage",
            value: FieldValue::Str(tables.name()),
        },
        Field {
            key: "stopped",
            value: FieldValue::UnsignedInt(stopped),
        },
        Field {
            key: "refused",
            value: FieldValue::UnsignedInt(refused),
        },
    ]
}

fn mastered(node: u32) -> [Field<'static>; 3] {
    [
        Field {
            key: "node",
            value: FieldValue::UnsignedInt(u64::from(node)),
        },
        Field {
            key: "master",
            value: FieldValue::Str("on"),
        },
        Field {
            key: "outcome",
            value: FieldValue::Str("applied"),
        },
    ]
}

const KEY: [Field<'static>; 1] = [Field {
    key: "kind",
    value: FieldValue::Str("key"),
}];

const REMAPPED: [Field<'static>; 1] = [Field {
    key: "outcome",
    value: FieldValue::Str("remapped"),
}];

/// Finds the keyboard, as the tree a vertical reads does.
const FOUND: fn() -> Option<u32> = || Some(KEYBOARD);

fn yes() -> bool {
    true
}

fn no() -> bool {
    false
}

#[test]
fn a_wired_run_passes_on_a_key_after_the_keyboard_is_granted_its_domain() {
    let witness = TranslationWitness::new(Interrupts::Wired, SECOND, Faults::Served);
    let translating = unit("translating", 0);
    assert_eq!(
        witness.observe(&event(AuditEvent::DmaTranslationUnit, &translating), FOUND),
        Verdict::Pending
    );
    let mouse = mastered(MOUSE);
    assert_eq!(
        witness.observe(&event(AuditEvent::DmaBusMaster, &mouse), FOUND),
        Verdict::Pending
    );
    let keys = mastered(KEYBOARD);
    assert_eq!(
        witness.observe(&event(AuditEvent::DmaBusMaster, &keys), FOUND),
        Verdict::Pending
    );
    assert_eq!(
        witness.observe(&event(AuditEvent::InputDelivered, &KEY), FOUND),
        Verdict::Pass
    );
}

#[test]
fn a_key_before_the_keyboard_is_granted_fails_the_run() {
    let witness = TranslationWitness::new(Interrupts::Wired, SECOND, Faults::Served);
    let translating = unit("translating", 0);
    witness.observe(&event(AuditEvent::DmaTranslationUnit, &translating), FOUND);
    let mouse = mastered(MOUSE);
    witness.observe(&event(AuditEvent::DmaBusMaster, &mouse), FOUND);
    assert_eq!(
        witness.observe(&event(AuditEvent::InputDelivered, &KEY), FOUND),
        Verdict::Fail(FAIL_EARLY_KEY),
        "the mouse's grant is not the keyboard's"
    );
}

#[test]
fn any_other_unit_outcome_or_a_fault_fails_the_run() {
    for (outcome, stopped) in [("translating", 1), ("faults_unrouted", 0), ("hardware", 0)] {
        let witness = TranslationWitness::new(Interrupts::Wired, SECOND, Faults::Served);
        let record = unit(outcome, stopped);
        assert_eq!(
            witness.observe(&event(AuditEvent::DmaTranslationUnit, &record), FOUND),
            Verdict::Fail(FAIL_UNIT),
            "{outcome}, {stopped} stopped"
        );
    }
    let witness = TranslationWitness::new(Interrupts::Wired, SECOND, Faults::Served);
    assert_eq!(
        witness.observe(&event(AuditEvent::DmaTranslationFault, &[]), FOUND),
        Verdict::Fail(FAIL_FAULT)
    );
}

#[test]
fn a_remapping_record_on_a_board_that_remaps_nothing_fails_the_run() {
    let witness = TranslationWitness::new(Interrupts::Wired, SECOND, Faults::Served);
    assert_eq!(
        witness.observe(&event(AuditEvent::InterruptRemapping, &REMAPPED), FOUND),
        Verdict::Fail(FAIL_REMAPPING)
    );
}

#[test]
fn a_remapping_board_needs_its_interrupts_remapped_before_any_grant() {
    let witness = TranslationWitness::new(
        Interrupts::Remapped { extended: yes },
        SECOND,
        Faults::Served,
    );
    let translating = unit("translating", 0);
    witness.observe(&event(AuditEvent::DmaTranslationUnit, &translating), FOUND);
    let keys = mastered(KEYBOARD);
    assert_eq!(
        witness.observe(&event(AuditEvent::DmaBusMaster, &keys), FOUND),
        Verdict::Fail(FAIL_EARLY_GRANT)
    );

    let witness = TranslationWitness::new(
        Interrupts::Remapped { extended: yes },
        SECOND,
        Faults::Served,
    );
    witness.observe(&event(AuditEvent::DmaTranslationUnit, &translating), FOUND);
    assert_eq!(
        witness.observe(&event(AuditEvent::InterruptRemapping, &REMAPPED), FOUND),
        Verdict::Pending
    );
    witness.observe(&event(AuditEvent::DmaBusMaster, &keys), FOUND);
    assert_eq!(
        witness.observe(&event(AuditEvent::InputDelivered, &KEY), FOUND),
        Verdict::Pass
    );

    let compatible = TranslationWitness::new(
        Interrupts::Remapped { extended: no },
        SECOND,
        Faults::Served,
    );
    compatible.observe(&event(AuditEvent::DmaTranslationUnit, &translating), FOUND);
    assert_eq!(
        compatible.observe(&event(AuditEvent::InterruptRemapping, &REMAPPED), FOUND),
        Verdict::Fail(FAIL_REMAPPING),
        "remapped, but not in the mode the CPUs take"
    );
}

#[test]
fn a_unit_at_another_stage_or_naming_none_fails_the_run() {
    let witness = TranslationWitness::new(Interrupts::Wired, FIRST, Faults::Served);
    let second = unit("translating", 0);
    assert_eq!(
        witness.observe(&event(AuditEvent::DmaTranslationUnit, &second), FOUND),
        Verdict::Fail(FAIL_STAGE),
        "a stage-1 run that translated at stage 2 proves nothing of stage 1"
    );
    let unnamed = [
        Field {
            key: "outcome",
            value: FieldValue::Str("translating"),
        },
        Field {
            key: "stopped",
            value: FieldValue::UnsignedInt(0),
        },
        Field {
            key: "refused",
            value: FieldValue::UnsignedInt(0),
        },
    ];
    assert_eq!(
        witness.observe(&event(AuditEvent::DmaTranslationUnit, &unnamed), FOUND),
        Verdict::Fail(FAIL_STAGE)
    );
    let first = record("translating", FIRST, 0, 0);
    assert_eq!(
        witness.observe(&event(AuditEvent::DmaTranslationUnit, &first), FOUND),
        Verdict::Pending
    );
}

#[test]
fn a_function_that_would_not_stop_fails_the_run() {
    let witness = TranslationWitness::new(Interrupts::Wired, SECOND, Faults::Served);
    let refused = record("translating", SECOND, 0, 1);
    assert_eq!(
        witness.observe(&event(AuditEvent::DmaTranslationUnit, &refused), FOUND),
        Verdict::Fail(FAIL_UNIT)
    );
}

#[test]
fn a_grant_before_the_unit_translates_fails_every_run() {
    let keys = mastered(KEYBOARD);
    for interrupts in [Interrupts::Wired, Interrupts::Remapped { extended: yes }] {
        let witness = TranslationWitness::new(interrupts, SECOND, Faults::Served);
        assert_eq!(
            witness.observe(&event(AuditEvent::DmaBusMaster, &keys), FOUND),
            Verdict::Fail(FAIL_EARLY_GRANT)
        );
    }
}

#[test]
fn remapping_before_the_unit_translates_fails_the_run() {
    let witness = TranslationWitness::new(
        Interrupts::Remapped { extended: yes },
        SECOND,
        Faults::Served,
    );
    assert_eq!(
        witness.observe(&event(AuditEvent::InterruptRemapping, &REMAPPED), FOUND),
        Verdict::Fail(FAIL_REMAPPING)
    );
}

const UNREMAPPED: [Field<'static>; 1] = [Field {
    key: "outcome",
    value: FieldValue::Str("unremapped"),
}];

/// A board whose unit cannot remap must say so before any grant, and saying
/// it remapped is as wrong as saying nothing.
#[test]
fn an_unremapping_board_records_its_routing_before_any_grant() {
    let translating = unit("translating", 0);
    let keys = mastered(KEYBOARD);
    let witness = TranslationWitness::new(Interrupts::Unremapped, SECOND, Faults::Served);
    witness.observe(&event(AuditEvent::DmaTranslationUnit, &translating), FOUND);
    assert_eq!(
        witness.observe(&event(AuditEvent::DmaBusMaster, &keys), FOUND),
        Verdict::Fail(FAIL_EARLY_GRANT)
    );
    let witness = TranslationWitness::new(Interrupts::Unremapped, SECOND, Faults::Served);
    witness.observe(&event(AuditEvent::DmaTranslationUnit, &translating), FOUND);
    assert_eq!(
        witness.observe(&event(AuditEvent::InterruptRemapping, &REMAPPED), FOUND),
        Verdict::Fail(FAIL_REMAPPING)
    );
    let witness = TranslationWitness::new(Interrupts::Unremapped, SECOND, Faults::Served);
    witness.observe(&event(AuditEvent::DmaTranslationUnit, &translating), FOUND);
    assert_eq!(
        witness.observe(&event(AuditEvent::InterruptRemapping, &UNREMAPPED), FOUND),
        Verdict::Pending
    );
    witness.observe(&event(AuditEvent::DmaBusMaster, &keys), FOUND);
    assert_eq!(
        witness.observe(&event(AuditEvent::InputDelivered, &KEY), FOUND),
        Verdict::Pass
    );
}

/// A unit that keeps its domains' translations itself is judged by that,
/// not by a stage of tables it walks.
#[test]
fn a_unit_keeping_its_own_translations_is_judged_by_that() {
    let witness = TranslationWitness::new(Interrupts::Wired, Tables::Kept, Faults::Served);
    let walked = unit("translating", 0);
    assert_eq!(
        witness.observe(&event(AuditEvent::DmaTranslationUnit, &walked), FOUND),
        Verdict::Fail(FAIL_STAGE)
    );
    let kept = record("translating", Tables::Kept, 0, 0);
    assert_eq!(
        witness.observe(&event(AuditEvent::DmaTranslationUnit, &kept), FOUND),
        Verdict::Pending
    );
}

fn unrouted(reason: &'static str) -> [Field<'static>; 3] {
    [
        Field {
            key: "node",
            value: FieldValue::UnsignedInt(7),
        },
        Field {
            key: "outcome",
            value: FieldValue::Str("faults_unrouted"),
        },
        Field {
            key: "reason",
            value: FieldValue::Str(reason),
        },
    ]
}

/// A unit nothing hears must say so, after it translates and once, for any
/// other reason failing the run; a run expecting its faults served fails on
/// it whatever the reason.
#[test]
fn a_unit_nothing_hears_says_so_once_and_only_where_expected() {
    let translating = record("translating", Tables::Kept, 0, 0);
    let keys = mastered(KEYBOARD);
    let no_line = unrouted("no_line");
    let run = || {
        let witness =
            TranslationWitness::new(Interrupts::Unremapped, Tables::Kept, Faults::Unheard);
        witness.observe(&event(AuditEvent::DmaTranslationUnit, &translating), FOUND);
        witness
    };
    let witness = run();
    assert_eq!(
        witness.observe(&event(AuditEvent::DmaTranslationUnit, &no_line), FOUND),
        Verdict::Pending
    );
    witness.observe(&event(AuditEvent::InterruptRemapping, &UNREMAPPED), FOUND);
    witness.observe(&event(AuditEvent::DmaBusMaster, &keys), FOUND);
    assert_eq!(
        witness.observe(&event(AuditEvent::InputDelivered, &KEY), FOUND),
        Verdict::Pass
    );

    let witness = run();
    witness.observe(&event(AuditEvent::InterruptRemapping, &UNREMAPPED), FOUND);
    witness.observe(&event(AuditEvent::DmaBusMaster, &keys), FOUND);
    assert_eq!(
        witness.observe(&event(AuditEvent::InputDelivered, &KEY), FOUND),
        Verdict::Fail(FAIL_UNIT),
        "the unit never said"
    );

    let witness = run();
    witness.observe(&event(AuditEvent::DmaTranslationUnit, &no_line), FOUND);
    assert_eq!(
        witness.observe(&event(AuditEvent::DmaTranslationUnit, &no_line), FOUND),
        Verdict::Fail(FAIL_UNIT),
        "twice"
    );
    let refused = unrouted("refused");
    assert_eq!(
        run().observe(&event(AuditEvent::DmaTranslationUnit, &refused), FOUND),
        Verdict::Fail(FAIL_UNIT)
    );
    let early = TranslationWitness::new(Interrupts::Unremapped, Tables::Kept, Faults::Unheard);
    assert_eq!(
        early.observe(&event(AuditEvent::DmaTranslationUnit, &no_line), FOUND),
        Verdict::Fail(FAIL_UNIT),
        "before it translates"
    );
    let served = TranslationWitness::new(Interrupts::Unremapped, Tables::Kept, Faults::Served);
    served.observe(&event(AuditEvent::DmaTranslationUnit, &translating), FOUND);
    assert_eq!(
        served.observe(&event(AuditEvent::DmaTranslationUnit, &no_line), FOUND),
        Verdict::Fail(FAIL_UNIT)
    );
}

const GIB: u64 = 1 << 30;
const UNIT: u32 = 25;

fn reported(node: u64) -> [Field<'static>; 2] {
    [
        Field {
            key: "outcome",
            value: FieldValue::Str("translating"),
        },
        Field {
            key: "node",
            value: FieldValue::UnsignedInt(node),
        },
    ]
}

fn unit_at(windows: &[u64]) -> HwNode {
    let mut node = HwNode::new(
        UNIT,
        tairix_abi::HW_NODE_ROOT_ID,
        tairix_abi::HwDeviceClass::Iommu,
    );
    node.push_resource(tairix_abi::HwResource::irq(7, 1))
        .expect("room for a line");
    for &base in windows {
        node.push_resource(tairix_abi::HwResource::mmio(base, 0x1000))
            .expect("room for a window");
    }
    node
}

#[test]
fn a_unit_translates_from_registers_at_or_above_its_floor() {
    let translating = reported(u64::from(UNIT));
    let report = event(AuditEvent::DmaTranslationUnit, &translating);
    let high = |node| (node == UNIT).then(|| unit_at(&[16 * GIB, 16 * GIB + 0x3000]));
    assert!(registers_from(&report, 4 * GIB, high));
    assert!(
        registers_from(&report, 16 * GIB, high),
        "the floor is inclusive"
    );
    assert!(
        !registers_from(&report, 4 * GIB, |_| Some(unit_at(&[
            16 * GIB,
            0x3000_0000
        ]))),
        "one window below"
    );
    assert!(registers_from(&report, 0, |_| Some(unit_at(&[0x301_0000]))));
}

#[test]
fn a_unit_with_no_window_or_no_node_fails_wherever_the_floor() {
    let translating = reported(u64::from(UNIT));
    let report = event(AuditEvent::DmaTranslationUnit, &translating);
    assert!(
        !registers_from(&report, 0, |_| Some(unit_at(&[]))),
        "a line alone"
    );
    assert!(!registers_from(&report, 0, |_| None), "not in the tree");
    let unnamed = [Field {
        key: "outcome",
        value: FieldValue::Str("translating"),
    }];
    assert!(!registers_from(
        &event(AuditEvent::DmaTranslationUnit, &unnamed),
        0,
        |_| Some(unit_at(&[GIB]))
    ));
    let wide = reported(u64::from(u32::MAX) + 1);
    assert!(!registers_from(
        &event(AuditEvent::DmaTranslationUnit, &wide),
        0,
        |_| Some(unit_at(&[GIB]))
    ));
}

#[test]
fn only_a_unit_translating_is_judged_by_its_registers() {
    let unasked = |_| -> Option<HwNode> { panic!("no unit is looked up") };
    let stopped = unit("stopped", 1);
    assert!(registers_from(
        &event(AuditEvent::DmaTranslationUnit, &stopped),
        4 * GIB,
        unasked
    ));
    assert!(registers_from(
        &event(AuditEvent::InputDelivered, &KEY),
        4 * GIB,
        unasked
    ));
}
