//! What the freestanding QEMU integration kernels' PASS witnesses read: an
//! audit record, and the fixtures the runner plants for them.
//!
//! A guest kernel's sink compiles only for its bare-metal target, where no
//! host test reaches it, so a reading every vertical shares lives here and is
//! proven once, as does a fixture the runner writes and a guest checks.
//!
//! Test scaffolding: nothing in TAIRiX itself links it.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tairix_log::{Event, EventId, FieldValue, Level};
use tairix_util::fmt::format_hex_u64;

/// The value `event` carries under `key`: the first field so named.
#[must_use]
pub fn field<'e>(event: &'e Event<'_>, key: &str) -> Option<&'e FieldValue<'e>> {
    event
        .fields
        .iter()
        .find(|field| field.key == key)
        .map(|field| &field.value)
}

/// The unsigned integer `event` carries under `key`: the first such field
/// holding one, or `None`.
#[must_use]
pub fn field_u64(event: &Event<'_>, key: &str) -> Option<u64> {
    event.fields.iter().find_map(|field| match field.value {
        FieldValue::UnsignedInt(value) if field.key == key => Some(value),
        _ => None,
    })
}

/// Byte `i` of the sector the runner plants at the start of a scratch disk.
#[must_use]
pub const fn sector0_byte(i: usize) -> u8 {
    i.to_le_bytes()[0]
}

/// The text `event` carries under `key`: the first such field holding a
/// string, or `None`.
#[must_use]
pub fn field_str<'e>(event: &Event<'e>, key: &str) -> Option<&'e str> {
    event.fields.iter().find_map(|field| match field.value {
        FieldValue::Str(value) if field.key == key => Some(value),
        _ => None,
    })
}

/// Whether `bundle` — an `APP_LOADED` record's `bundle` field — is the bundle
/// `<store>/<name>.app`, spelled with the shared `lib/abi` suffix rather than
/// a path written out.
#[must_use]
pub fn names_bundle(bundle: &str, store: &str, name: &str) -> bool {
    bundle
        .strip_prefix(store)
        .and_then(|rest| rest.strip_prefix('/'))
        .and_then(|leaf| leaf.strip_suffix(tairix_abi::BUNDLE_SUFFIX))
        .is_some_and(|leaf| leaf == name)
}

/// The command word the login shell runs as.
pub const LOGIN_SHELL: &str = "elsh";

/// A scripted vertical's end: the login shell's audited exit after a fixture's.
///
/// The record names no exit status, so a failing fixture's exit arms it too;
/// what keeps a failure from passing is the script, which types the shell's
/// `exit` only once the fixture has printed its pass, so a fixture that fails
/// leaves the run to time out with its reason on serial. No other process's
/// exit ends the run, and ending it on the fixture's own would leave the
/// script's last line owed.
pub struct FixtureExit {
    fixtures: &'static [&'static str],
    syscall_invoked: EventId,
    exited: AtomicBool,
}

impl FixtureExit {
    /// Neither exit in, for a run whose fixture runs as any of `fixtures`,
    /// read off the kernel's audited `syscall_invoked` record.
    #[must_use]
    pub const fn new(fixtures: &'static [&'static str], syscall_invoked: EventId) -> Self {
        Self {
            fixtures,
            syscall_invoked,
            exited: AtomicBool::new(false),
        }
    }

    /// Read `event`, answering whether it is the shell's exit after a
    /// fixture's.
    pub fn passed(&self, event: &Event<'_>) -> bool {
        if event.id != self.syscall_invoked || field_str(event, "sc") != Some("exit") {
            return false;
        }
        match field_str(event, "comm") {
            Some(comm) if self.fixtures.contains(&comm) => {
                self.exited.store(true, Ordering::Release);
                false
            }
            Some(LOGIN_SHELL) => self.exited.load(Ordering::Acquire),
            _ => false,
        }
    }
}

/// What a guest sink prints on serial as a progress marker: an event carrying
/// `message` alone, which is the whole fact.
#[must_use]
pub const fn marker(message: &'static str) -> Event<'static> {
    Event {
        level: Level::Info,
        id: EventId(0),
        message,
        fields: &[],
    }
}

/// One step of a [`SyscallChain`]: the process the kernel attests and the
/// syscall it dispatched, as a dispatched-syscall record's `comm` and `sc`
/// fields render them.
pub type ChainStep = (&'static str, &'static str);

/// What one record did to a [`SyscallChain`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ChainNote {
    /// It is not the record the chain is owed, and changed nothing.
    Unrelated,
    /// It latched a step.
    Latched {
        /// The rounds the chain had completed before it.
        round: u32,
        /// The step's index in the chain.
        step: usize,
    },
    /// It was the owed instance step made by a task other than the instance
    /// the chain watches, so the round starts again.
    Foreign,
}

/// Dispatched syscalls that must land in a fixed order, round after round, one
/// step of every round made by one kernel-attested task.
///
/// A record that is not the one owed is ignored, because unrelated calls
/// separate the steps of a real chain: what is asserted is their relative
/// order. The task first making the instance step is captured, and only it
/// completes a round after, so the rounds are one instance's rather than any
/// process's of that name.
pub struct SyscallChain {
    chain: &'static [ChainStep],
    instance_step: usize,
    /// The rounds completed in the high half and the step owed in the low, in
    /// one word, so a record moves both or neither.
    position: AtomicU64,
    /// The instance's task, or [`NO_TASK`] before the instance step latched.
    instance: AtomicU64,
}

/// No task has made the instance step. Task 0 is the in-kernel system context,
/// which makes no user-space syscall, so no record names it.
const NO_TASK: u64 = 0;

/// Bits of a [`SyscallChain`] position below its rounds: the step owed.
const STEP_BITS: u32 = 32;

/// The position `round` rounds in, with step `step` owed.
fn pack(round: u32, step: u32) -> u64 {
    (u64::from(round) << STEP_BITS) | u64::from(step)
}

/// The rounds completed and the step owed at `position`.
fn unpack(position: u64) -> (u32, u32) {
    let round = u32::try_from(position >> STEP_BITS).unwrap_or(u32::MAX);
    let step = u32::try_from(position & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    (round, step)
}

impl SyscallChain {
    /// A chain owed its first step, its step `instance_step` made by one task
    /// every round.
    #[must_use]
    pub const fn new(chain: &'static [ChainStep], instance_step: usize) -> Self {
        Self {
            chain,
            instance_step,
            position: AtomicU64::new(0),
            instance: AtomicU64::new(NO_TASK),
        }
    }

    /// Read the dispatched-syscall record `event`, answering what it did.
    ///
    /// The process, the syscall and the task are the record's own
    /// kernel-attested fields, so no principal can present itself as another.
    /// A record making the instance step whose task cannot be read is refused
    /// rather than taken as a match.
    pub fn note(&self, event: &Event<'_>) -> ChainNote {
        let at = self.position.load(Ordering::Acquire);
        let (round, step) = unpack(at);
        let index = usize::try_from(step).unwrap_or(usize::MAX);
        let Some(&(comm, call)) = self.chain.get(index) else {
            return ChainNote::Unrelated;
        };
        if field_str(event, "comm") != Some(comm) || field_str(event, "sc") != Some(call) {
            return ChainNote::Unrelated;
        }
        if index == self.instance_step && !self.is_instance(field_str(event, "task")) {
            // A record that moved the chain meanwhile stands.
            let _ = self.position.compare_exchange(
                at,
                pack(round, 0),
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return ChainNote::Foreign;
        }
        let next = if index + 1 < self.chain.len() {
            pack(round, step + 1)
        } else {
            pack(round.saturating_add(1), 0)
        };
        match self
            .position
            .compare_exchange(at, next, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => ChainNote::Latched { round, step: index },
            // Another record latched the step first.
            Err(_) => ChainNote::Unrelated,
        }
    }

    /// The rounds the chain has completed.
    #[must_use]
    pub fn rounds(&self) -> u32 {
        unpack(self.position.load(Ordering::Acquire)).0
    }

    /// Whether `task`, the hex task field of a record making the instance step,
    /// names the instance: captured if none is yet.
    fn is_instance(&self, task: Option<&str>) -> bool {
        let Some(task) = task.and_then(|task| u64::from_str_radix(task, 16).ok()) else {
            return false;
        };
        if task == NO_TASK {
            return false;
        }
        match self
            .instance
            .compare_exchange(NO_TASK, task, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => true,
            Err(captured) => captured == task,
        }
    }
}

/// The login screen's witnesses that its frame reached a display: the system
/// service store's greeter bundle was verified and loaded, and then the
/// process the kernel names as serving the display endpoint reported the first
/// client frame its display took — a report the service makes only once a
/// present was accepted, and the kernel attributes to its emitter. The greeter
/// is the only graphical client alive at that point, so the frame is its own;
/// ordering it after the load is what attributes it.
pub struct GreeterFrame {
    loaded: AtomicBool,
    /// The process serving the display endpoint, [`NO_SERVER`] until the
    /// kernel names one.
    display: AtomicU64,
    presented: AtomicBool,
}

/// No process is known to serve the display endpoint.
const NO_SERVER: u64 = u64::MAX;

/// The records a [`GreeterFrame`] reads.
#[derive(Copy, Clone, Debug)]
pub struct GreeterRecords {
    /// The bundle loader's record of a verified, loaded bundle.
    pub app_loaded: EventId,
    /// The IPC layer's record of a call endpoint bound, naming its process.
    pub endpoint_created: EventId,
    /// The display service's record of the first frame its display took.
    pub first_present: EventId,
}

impl GreeterFrame {
    /// Neither witness in.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            loaded: AtomicBool::new(false),
            display: AtomicU64::new(NO_SERVER),
            presented: AtomicBool::new(false),
        }
    }

    /// Read `event`, an audit record or a process's own record as the kernel
    /// passes it on, answering whether both witnesses are in.
    pub fn observe(&self, event: &Event<'_>, records: GreeterRecords) -> bool {
        if event.id == records.app_loaded {
            let greeter = tairix_abi::GREETER_RUN_PATH
                .rsplit_once('/')
                .map(|(bundle, _)| bundle);
            if greeter.is_some() && field_str(event, "bundle") == greeter {
                self.loaded.store(true, Ordering::Release);
            }
        } else if event.id == records.endpoint_created {
            let mut hex = [0u8; 16];
            let endpoint = format_hex_u64(tairix_abi::display_ipc::DISPLAY_ENDPOINT, &mut hex);
            if let (Some(bound), Some(process)) =
                (field_str(event, "endpoint"), field_u64(event, "process"))
            {
                if bound == endpoint {
                    self.display.store(process, Ordering::Release);
                }
            }
        } else if event.id == records.first_present && self.loaded.load(Ordering::Acquire) {
            let server = self.display.load(Ordering::Acquire);
            // The kernel stamps a process's record with its emitter first.
            if server != NO_SERVER && field_u64(event, "task") == Some(server) {
                self.presented.store(true, Ordering::Release);
            }
        }
        self.loaded.load(Ordering::Acquire) && self.presented.load(Ordering::Acquire)
    }
}

impl Default for GreeterFrame {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::format;

    use super::{
        field, field_str, field_u64, marker, names_bundle, sector0_byte, ChainNote, ChainStep,
        FixtureExit, GreeterFrame, GreeterRecords, SyscallChain, LOGIN_SHELL,
    };
    use tairix_abi::{SYSTEM_APPLICATION_STORE, SYSTEM_SERVICE_STORE};
    use tairix_log::{Event, EventId, Field, FieldValue, Level};

    /// The audited exit of the process running as `comm`.
    fn exit_of(comm: &'static str, run: impl FnOnce(&Event<'_>)) {
        let fields = [
            Field {
                key: "sc",
                value: FieldValue::Str("exit"),
            },
            Field {
                key: "comm",
                value: FieldValue::Str(comm),
            },
        ];
        run(&Event {
            level: Level::Debug,
            id: EventId(5000),
            message: "",
            fields: &fields,
        });
    }

    #[test]
    fn a_vertical_ends_on_the_shell_s_exit_after_its_fixture_s_and_on_nothing_else() {
        let end = FixtureExit::new(&["images"], EventId(5000));
        exit_of(LOGIN_SHELL, |event| {
            assert!(!end.passed(event), "the shell before the fixture");
        });
        exit_of("mount", |event| assert!(!end.passed(event)));
        exit_of("images", |event| {
            assert!(!end.passed(event), "the fixture's own exit");
        });
        exit_of("unmount", |event| {
            assert!(!end.passed(event), "another program's exit");
        });
        exit_of(LOGIN_SHELL, |event| assert!(end.passed(event)));
        let other = FixtureExit::new(&["images"], EventId(4999));
        exit_of("images", |event| assert!(!other.passed(event)));
        exit_of(LOGIN_SHELL, |event| {
            assert!(!other.passed(event), "another record names no exit");
        });
    }

    #[test]
    fn any_of_a_run_s_fixtures_arms_it_and_each_of_their_exits_counts_as_one() {
        let end = FixtureExit::new(&["audiotone", "play"], EventId(5000));
        exit_of("play", |event| assert!(!end.passed(event)));
        exit_of("play", |event| {
            assert!(
                !end.passed(event),
                "the player's decoder worker is the player"
            );
        });
        exit_of("soundd", |event| {
            assert!(!end.passed(event), "a service's exit is not the shell's");
        });
        exit_of(LOGIN_SHELL, |event| assert!(end.passed(event)));
        let tone = FixtureExit::new(&["audiotone", "play"], EventId(5000));
        exit_of("audiotone", |event| assert!(!tone.passed(event)));
        exit_of(LOGIN_SHELL, |event| assert!(tone.passed(event)));
    }

    #[test]
    fn a_value_reads_as_the_first_field_under_its_key() {
        let fields = [
            Field {
                key: "stopped",
                value: FieldValue::UnsignedInt(2),
            },
            Field {
                key: "stopped",
                value: FieldValue::Str("later"),
            },
        ];
        let event = Event {
            level: Level::Info,
            id: EventId(1),
            message: "",
            fields: &fields,
        };
        assert_eq!(field(&event, "stopped"), Some(&FieldValue::UnsignedInt(2)));
        assert_eq!(field(&event, "absent"), None);
    }

    #[test]
    fn a_field_reads_as_the_first_text_under_its_key() {
        let fields = [
            Field {
                key: "kind",
                value: FieldValue::UnsignedInt(3),
            },
            Field {
                key: "kind",
                value: FieldValue::Str("touch"),
            },
            Field {
                key: "kind",
                value: FieldValue::Str("key"),
            },
        ];
        let event = Event {
            level: Level::Info,
            id: EventId(1),
            message: "",
            fields: &fields,
        };
        assert_eq!(field_str(&event, "kind"), Some("touch"));
        assert_eq!(field_str(&event, "bundle"), None);
    }

    #[test]
    fn a_field_reads_as_the_first_unsigned_integer_under_its_key() {
        let fields = [
            Field {
                key: "iova",
                value: FieldValue::Str("high"),
            },
            Field {
                key: "iova",
                value: FieldValue::UnsignedInt(0x1000),
            },
            Field {
                key: "iova",
                value: FieldValue::UnsignedInt(0x2000),
            },
        ];
        let event = Event {
            level: Level::Info,
            id: EventId(1),
            message: "",
            fields: &fields,
        };
        assert_eq!(field_u64(&event, "iova"), Some(0x1000));
        assert_eq!(field_u64(&event, "stream"), None);
    }

    #[test]
    fn the_planted_sector_counts_each_byte_s_offset_modulo_256() {
        assert_eq!(sector0_byte(0), 0);
        assert_eq!(sector0_byte(255), 255);
        assert_eq!(sector0_byte(256), 0);
        assert_eq!(sector0_byte(511), 255);
    }

    #[test]
    fn a_bundle_is_named_only_by_its_own_store_name_and_suffix() {
        let store = SYSTEM_APPLICATION_STORE;
        let bundle = |path: &str| names_bundle(path, store, "terminal");
        assert!(bundle(&format!("{store}/terminal.app")));
        assert!(!bundle(&format!("{store}/terminals.app")));
        assert!(!bundle(&format!("{store}/terminal")));
        assert!(
            !bundle(&format!("{store}/games/terminal.app")),
            "a nested bundle"
        );
        assert!(!bundle(&format!("{store}x/terminal.app")), "a longer store");
        assert!(!bundle(&format!("{store}terminal.app")), "no separator");
        assert!(!names_bundle(
            &format!("{SYSTEM_SERVICE_STORE}/terminal.app"),
            store,
            "terminal"
        ));
        assert!(!bundle(""));
    }

    #[test]
    fn a_greeter_frame_is_the_display_server_s_first_frame_after_the_greeter_loaded() {
        const RECORDS: GreeterRecords = GreeterRecords {
            app_loaded: EventId(11_001),
            endpoint_created: EventId(3_040),
            first_present: EventId(15_001),
        };
        let event = |id, fields: &'static [Field<'static>]| Event {
            level: Level::Info,
            id,
            message: "",
            fields,
        };
        let leak = |fields: std::vec::Vec<Field<'static>>| -> &'static [Field<'static>] {
            std::boxed::Box::leak(fields.into_boxed_slice())
        };
        let text = |value: std::string::String| -> &'static str {
            std::boxed::Box::leak(value.into_boxed_str())
        };
        let greeter = text(format!("{SYSTEM_SERVICE_STORE}/greeter.app"));
        let display = text(format!(
            "{:016x}",
            tairix_abi::display_ipc::DISPLAY_ENDPOINT
        ));
        let loaded = leak(std::vec![Field {
            key: "bundle",
            value: FieldValue::Str(greeter),
        }]);
        let bound = |endpoint: &'static str, process: u64| {
            leak(std::vec![
                Field {
                    key: "endpoint",
                    value: FieldValue::Str(endpoint),
                },
                Field {
                    key: "process",
                    value: FieldValue::UnsignedInt(process),
                },
            ])
        };
        let shown_by = |task: u64| {
            leak(std::vec![Field {
                key: "task",
                value: FieldValue::UnsignedInt(task),
            }])
        };
        let witness = GreeterFrame::new();
        let observe = |id, fields| witness.observe(&event(id, fields), RECORDS);
        assert!(!observe(RECORDS.endpoint_created, bound(display, 40)));
        assert!(
            !observe(RECORDS.first_present, shown_by(40)),
            "a frame before the greeter is nobody's"
        );
        assert!(!observe(RECORDS.app_loaded, loaded));
        assert!(
            !observe(RECORDS.first_present, shown_by(41)),
            "a report from a process not serving the display proves nothing"
        );
        let other = text(format!("{:016x}", 7));
        assert!(!observe(RECORDS.endpoint_created, bound(other, 41)));
        assert!(!observe(RECORDS.first_present, shown_by(41)));
        assert!(observe(RECORDS.first_present, shown_by(40)));
    }

    /// The steps a session-to-viewer delegation is recognised by.
    const DELEGATION: [ChainStep; 2] = [("desktop", "fd_grant"), ("view", "fd_redeem")];

    /// What `chain` makes of the dispatched-syscall record of the process
    /// `comm` making `call` from task `task`.
    fn note(
        chain: &SyscallChain,
        comm: &'static str,
        call: &'static str,
        task: &'static str,
    ) -> ChainNote {
        let fields = [
            Field {
                key: "task",
                value: FieldValue::Str(task),
            },
            Field {
                key: "comm",
                value: FieldValue::Str(comm),
            },
            Field {
                key: "sc",
                value: FieldValue::Str(call),
            },
        ];
        chain.note(&Event {
            level: Level::Debug,
            id: EventId(5000),
            message: "",
            fields: &fields,
        })
    }

    #[test]
    fn a_chain_latches_its_steps_in_order_past_unrelated_records() {
        let chain = SyscallChain::new(&DELEGATION, 1);
        assert_eq!(
            note(&chain, "view", "fd_redeem", "2a"),
            ChainNote::Unrelated,
            "a redeem before any grant"
        );
        assert_eq!(
            note(&chain, "desktop", "fd_grant", "1"),
            ChainNote::Latched { round: 0, step: 0 }
        );
        assert_eq!(
            note(&chain, "files", "fd_redeem", "3"),
            ChainNote::Unrelated,
            "another principal's redeem"
        );
        assert_eq!(
            note(&chain, "desktop", "fd_grant", "1"),
            ChainNote::Unrelated,
            "a latched step is owed no more"
        );
        assert_eq!(
            note(&chain, "view", "fd_redeem", "2a"),
            ChainNote::Latched { round: 0, step: 1 }
        );
        assert_eq!(chain.rounds(), 1);
        assert_eq!(
            note(&chain, "desktop", "fd_grant", "1"),
            ChainNote::Latched { round: 1, step: 0 }
        );
        assert_eq!(chain.rounds(), 1, "a round counts once it ends");
    }

    #[test]
    fn only_the_captured_instance_completes_a_later_round() {
        let chain = SyscallChain::new(&DELEGATION, 1);
        for (comm, call) in DELEGATION {
            assert!(matches!(
                note(&chain, comm, call, "2a"),
                ChainNote::Latched { round: 0, .. }
            ));
        }
        assert_eq!(
            note(&chain, "desktop", "fd_grant", "1"),
            ChainNote::Latched { round: 1, step: 0 }
        );
        assert_eq!(note(&chain, "view", "fd_redeem", "2b"), ChainNote::Foreign);
        assert_eq!(chain.rounds(), 1, "a second viewer completes nothing");
        assert_eq!(
            note(&chain, "view", "fd_redeem", "2a"),
            ChainNote::Unrelated,
            "the round started again, so a grant is owed"
        );
        for (comm, call) in DELEGATION {
            assert!(matches!(
                note(&chain, comm, call, "2a"),
                ChainNote::Latched { round: 1, .. }
            ));
        }
        assert_eq!(chain.rounds(), 2);
    }

    #[test]
    fn an_instance_step_naming_no_readable_task_is_refused() {
        for task in ["", "not hex", "0"] {
            let chain = SyscallChain::new(&DELEGATION, 1);
            assert_eq!(
                note(&chain, "desktop", "fd_grant", "1"),
                ChainNote::Latched { round: 0, step: 0 }
            );
            assert_eq!(
                note(&chain, "view", "fd_redeem", task),
                ChainNote::Foreign,
                "{task:?}"
            );
            assert_eq!(chain.rounds(), 0);
        }
    }

    #[test]
    fn a_marker_carries_its_message_and_nothing_else() {
        let event = marker("FILEPICKS step");
        assert_eq!(event.message, "FILEPICKS step");
        assert_eq!(event.level, Level::Info);
        assert!(event.fields.is_empty());
    }
}
