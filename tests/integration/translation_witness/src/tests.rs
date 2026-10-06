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

fn unit(outcome: &'static str, stopped: u64) -> [Field<'static>; 4] {
    record(outcome, Stage::Second, stopped, 0)
}

fn record(outcome: &'static str, stage: Stage, stopped: u64, refused: u64) -> [Field<'static>; 4] {
    [
        Field {
            key: "outcome",
            value: FieldValue::Str(outcome),
        },
        Field {
            key: "stage",
            value: FieldValue::Str(stage.name()),
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
    let witness = TranslationWitness::new(Interrupts::Wired, Stage::Second);
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
    let witness = TranslationWitness::new(Interrupts::Wired, Stage::Second);
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
        let witness = TranslationWitness::new(Interrupts::Wired, Stage::Second);
        let record = unit(outcome, stopped);
        assert_eq!(
            witness.observe(&event(AuditEvent::DmaTranslationUnit, &record), FOUND),
            Verdict::Fail(FAIL_UNIT),
            "{outcome}, {stopped} stopped"
        );
    }
    let witness = TranslationWitness::new(Interrupts::Wired, Stage::Second);
    assert_eq!(
        witness.observe(&event(AuditEvent::DmaTranslationFault, &[]), FOUND),
        Verdict::Fail(FAIL_FAULT)
    );
}

#[test]
fn a_remapping_record_on_a_board_that_remaps_nothing_fails_the_run() {
    let witness = TranslationWitness::new(Interrupts::Wired, Stage::Second);
    assert_eq!(
        witness.observe(&event(AuditEvent::InterruptRemapping, &REMAPPED), FOUND),
        Verdict::Fail(FAIL_REMAPPING)
    );
}

#[test]
fn a_remapping_board_needs_its_interrupts_remapped_before_any_grant() {
    let witness = TranslationWitness::new(Interrupts::Remapped { extended: yes }, Stage::Second);
    let translating = unit("translating", 0);
    witness.observe(&event(AuditEvent::DmaTranslationUnit, &translating), FOUND);
    let keys = mastered(KEYBOARD);
    assert_eq!(
        witness.observe(&event(AuditEvent::DmaBusMaster, &keys), FOUND),
        Verdict::Fail(FAIL_EARLY_GRANT)
    );

    let witness = TranslationWitness::new(Interrupts::Remapped { extended: yes }, Stage::Second);
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

    let compatible = TranslationWitness::new(Interrupts::Remapped { extended: no }, Stage::Second);
    compatible.observe(&event(AuditEvent::DmaTranslationUnit, &translating), FOUND);
    assert_eq!(
        compatible.observe(&event(AuditEvent::InterruptRemapping, &REMAPPED), FOUND),
        Verdict::Fail(FAIL_REMAPPING),
        "remapped, but not in the mode the CPUs take"
    );
}

#[test]
fn a_unit_at_another_stage_or_naming_none_fails_the_run() {
    let witness = TranslationWitness::new(Interrupts::Wired, Stage::First);
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
    let first = record("translating", Stage::First, 0, 0);
    assert_eq!(
        witness.observe(&event(AuditEvent::DmaTranslationUnit, &first), FOUND),
        Verdict::Pending
    );
}

#[test]
fn a_function_that_would_not_stop_fails_the_run() {
    let witness = TranslationWitness::new(Interrupts::Wired, Stage::Second);
    let refused = record("translating", Stage::Second, 0, 1);
    assert_eq!(
        witness.observe(&event(AuditEvent::DmaTranslationUnit, &refused), FOUND),
        Verdict::Fail(FAIL_UNIT)
    );
}

#[test]
fn a_grant_before_the_unit_translates_fails_every_run() {
    let keys = mastered(KEYBOARD);
    for interrupts in [Interrupts::Wired, Interrupts::Remapped { extended: yes }] {
        let witness = TranslationWitness::new(interrupts, Stage::Second);
        assert_eq!(
            witness.observe(&event(AuditEvent::DmaBusMaster, &keys), FOUND),
            Verdict::Fail(FAIL_EARLY_GRANT)
        );
    }
}

#[test]
fn remapping_before_the_unit_translates_fails_the_run() {
    let witness = TranslationWitness::new(Interrupts::Remapped { extended: yes }, Stage::Second);
    assert_eq!(
        witness.observe(&event(AuditEvent::InterruptRemapping, &REMAPPED), FOUND),
        Verdict::Fail(FAIL_REMAPPING)
    );
}
