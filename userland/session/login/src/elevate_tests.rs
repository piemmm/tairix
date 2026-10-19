//! Decision-table tests for the elevation broker.
//!
//! Split from `elevate.rs` to keep either file inside the 500-line bound.

extern crate std;

use super::{
    audit_launch_ended_abnormally, drain_bounded, handle_elevate_request, AuthoriserPage,
    Authority, Caller, Captured, ElevateDirectory, ElevateLauncher, ProgramRequests,
};
use crate::budget::{AttemptBudget, FREE_ATTEMPTS};
use crate::events;
use crate::session::{AuthenticatedUser, Authenticator, Credentials, Gid, Uid};
use alloc::string::ToString;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use tairix_abi::elevate::{
    ElevateArgv, ElevateReply, ElevateRequest, ELEVATE_MAX_ARGS, ELEVATE_MAX_OUTPUT,
    ELEVATE_MAX_REPLY, ELEVATE_MAX_REQUEST,
};
use tairix_abi::session_ipc::{decode_account_page, SessionAccount, DESKTOP_BUNDLE};
use tairix_abi::time::Duration64;
use tairix_abi::{AppIdentity, CapabilityId, Errno, PublisherId, LOAD_UNVERIFIED};
use tairix_caps::CapabilitySet;
use tairix_log::{Event, EventId, Sink};

/// One decided request, kept as the frame the supervisor would post.
///
/// The broker's answer borrows the scratch it drained a captured run
/// into, so a test that holds two verdicts at once cannot hold two
/// borrows of one scratch; keeping the encoded frame instead owns the
/// answer, and round-trips every broker decision through the wire
/// encoding on the way.
#[derive(Clone)]
struct Decided(Vec<u8>);

impl Decided {
    fn reply(&self) -> ElevateReply<'_> {
        ElevateReply::decode(&self.0).expect("the supervisor's own frame decodes")
    }
}

impl core::fmt::Debug for Decided {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.reply().fmt(f)
    }
}

impl PartialEq for Decided {
    fn eq(&self, other: &Self) -> bool {
        self.reply() == other.reply()
    }
}

impl PartialEq<ElevateReply<'_>> for Decided {
    fn eq(&self, other: &ElevateReply<'_>) -> bool {
        self.reply() == *other
    }
}

/// The OS signing key's publisher, which login and the desktop share.
const SYSTEM: PublisherId = PublisherId::from_raw([0x05; 32]);

fn identity(bundle: &str, publisher: PublisherId) -> AppIdentity {
    AppIdentity::new(bundle, publisher).expect("a well-formed identity")
}

/// Scripted manifests: what each listed program requests; any other
/// program has none to read.
struct Manifests(Vec<(&'static str, CapabilitySet)>);

impl Manifests {
    /// Every program these tests name, each requesting nothing.
    fn shipped() -> Self {
        Self(alloc::vec![
            ("/System/Commands/users.app/Run", CapabilitySet::empty()),
            ("/System/Commands/configure.app/Run", CapabilitySet::empty()),
            ("/System/Commands/cat.app/Run", CapabilitySet::empty()),
            (tairix_abi::DATETIME_RUN_PATH, CapabilitySet::empty()),
        ])
    }

    /// The same, with `program` requesting `ids`.
    fn with(mut self, program: &'static str, ids: &[CapabilityId]) -> Self {
        self.0.retain(|(listed, _)| *listed != program);
        self.0.push((program, caps(ids)));
        self
    }
}

impl ProgramRequests for Manifests {
    fn requested(&self, program: &str) -> Result<CapabilitySet, Errno> {
        self.0
            .iter()
            .find(|(listed, _)| *listed == program)
            .map(|(_, caps)| *caps)
            .ok_or(Errno::NotFound)
    }
}

/// The account view: `root` as uid 0, who presents, and a scripted
/// answer to the listing, recording what it was asked.
struct Directory {
    presenting: Option<u32>,
    page: AuthoriserPage,
    asked: RefCell<Vec<(CapabilitySet, u32, u32)>>,
}

impl Default for Directory {
    fn default() -> Self {
        Self {
            presenting: None,
            page: AuthoriserPage {
                own: true,
                total: 1,
                accounts: alloc::vec![SessionAccount::new("System Administrator", "root", true)
                    .expect("a tile the wire carries")],
            },
            asked: RefCell::new(Vec::new()),
        }
    }
}

impl ElevateDirectory for Directory {
    fn login_name(&self, uid: u32) -> Option<&str> {
        (uid == 0).then_some("root")
    }

    fn presenting(&self) -> Option<u32> {
        self.presenting
    }

    fn authorisers(&self, requested: &CapabilitySet, caller: u32, offset: u32) -> AuthoriserPage {
        self.asked.borrow_mut().push((*requested, caller, offset));
        self.page.clone()
    }
}

/// Everything one test decides requests through, the meter and the clock
/// kept across requests so a sequence of them is metered as one caller's.
struct Bench<'t> {
    authenticator: &'t dyn Authenticator,
    launcher: &'t dyn ElevateLauncher,
    sink: &'t dyn Sink,
    programs: Manifests,
    directory: Directory,
    own_app: Option<AppIdentity>,
    meter: RefCell<AttemptBudget>,
    now: Cell<Duration64>,
}

/// The console login serves in these tests.
const OWN_CONSOLE: u64 = 1;

impl<'t> Bench<'t> {
    fn new(
        authenticator: &'t dyn Authenticator,
        launcher: &'t dyn ElevateLauncher,
        sink: &'t dyn Sink,
    ) -> Self {
        Self {
            authenticator,
            launcher,
            sink,
            programs: Manifests::shipped(),
            directory: Directory::default(),
            own_app: Some(identity("os.tairix.login", SYSTEM)),
            meter: RefCell::new(AttemptBudget::new()),
            now: Cell::new(Duration64::ZERO),
        }
    }

    /// Decide one request from `caller` against a fresh output scratch.
    fn decide(&self, bytes: &[u8], caller: Caller<'_>) -> Decided {
        let mut out = [0u8; ELEVATE_MAX_OUTPUT];
        let mut frame = [0u8; ELEVATE_MAX_REPLY];
        let reply = handle_elevate_request(
            bytes,
            &caller,
            &Authority {
                console: OWN_CONSOLE,
                app: self.own_app.as_ref(),
                authenticator: self.authenticator,
                launcher: self.launcher,
                programs: &self.programs,
                directory: &self.directory,
                sink: self.sink,
            },
            &mut self.meter.borrow_mut(),
            self.now.get(),
            &mut out,
        );
        let len = reply
            .encode(&mut frame)
            .expect("the supervisor's answer encodes");
        Decided(frame[..len].to_vec())
    }
}

/// A caller on `console` attested as `uid`, running no bundle.
const fn shell(console: u64, uid: Option<u32>) -> Caller<'static> {
    Caller {
        console,
        uid,
        app: None,
    }
}

/// Decide one request from a bundle-less caller against a fresh bench.
fn decide(
    bytes: &[u8],
    peer_console: u64,
    peer_uid: Option<u32>,
    authenticator: &dyn Authenticator,
    launcher: &dyn ElevateLauncher,
    sink: &dyn Sink,
) -> Decided {
    Bench::new(authenticator, launcher, sink).decide(bytes, shell(peer_console, peer_uid))
}

/// What `root` may allow: account administration and setting the clock.
const ROOT_CEILING: &[CapabilityId] = &[CapabilityId::USER_ADMIN, CapabilityId::TIME_SET];

fn caps(ids: &[CapabilityId]) -> CapabilitySet {
    let mut set = CapabilitySet::empty();
    for id in ids {
        set.insert(*id);
    }
    set
}

/// Authenticator accepting exactly `root`/`correct` as uid 0, by name or
/// by uid.
struct FixedAuth;

impl FixedAuth {
    fn root() -> AuthenticatedUser {
        AuthenticatedUser {
            username: "root".to_string(),
            shown_name: "System Administrator".to_string(),
            uid: Uid(0),
            primary_gid: Gid(0),
            supplementary_gids: Vec::new(),
            capabilities: caps(ROOT_CEILING),
            home: "/Users/root".to_string(),
            shell: "/System/Services/shell".to_string(),
        }
    }
}

impl Authenticator for FixedAuth {
    fn authenticate(&self, credentials: &Credentials<'_>) -> Result<AuthenticatedUser, Errno> {
        // Unknown accounts, wrong passwords, and locked accounts are one
        // indistinguishable refusal, exactly as the production
        // `UsersAuthenticator` behaves.
        if credentials.username == "root" && credentials.password == "correct" {
            Ok(Self::root())
        } else {
            Err(Errno::PermissionDenied)
        }
    }

    fn authenticate_uid(&self, uid: u32, password: &str) -> Result<AuthenticatedUser, Errno> {
        // Mirrors `authenticate`: an unresolvable uid and a wrong
        // password on the real one are the identical refusal.
        if uid == 0 && password == "correct" {
            Ok(Self::root())
        } else {
            Err(Errno::PermissionDenied)
        }
    }
}

/// One recorded run: the program, the arguments it was handed, and the
/// account it ran as.
type RecordedRun = (alloc::string::String, Vec<alloc::string::String>, u32);

/// Launcher recording each run, launch, and capture, and returning a
/// scripted outcome for all three.
struct MockLauncher {
    outcome: Result<i32, Errno>,
    /// What a captured run prints; longer than the caller's scratch
    /// models a program that overruns the seam.
    printed: Vec<u8>,
    runs: RefCell<Vec<RecordedRun>>,
    launches: RefCell<Vec<(alloc::string::String, u32)>>,
    captures: RefCell<Vec<RecordedRun>>,
}

impl MockLauncher {
    fn new(outcome: Result<i32, Errno>) -> Self {
        Self {
            outcome,
            printed: Vec::new(),
            runs: RefCell::new(Vec::new()),
            launches: RefCell::new(Vec::new()),
            captures: RefCell::new(Vec::new()),
        }
    }

    /// The same launcher, with a captured run printing `printed`.
    fn printing(outcome: Result<i32, Errno>, printed: &[u8]) -> Self {
        Self {
            printed: printed.to_vec(),
            ..Self::new(outcome)
        }
    }
}

impl ElevateLauncher for MockLauncher {
    fn run_as(&self, program: &str, argv: ElevateArgv<'_>, uid: u32) -> Result<i32, Errno> {
        self.runs.borrow_mut().push((
            program.to_string(),
            argv.iter().map(ToString::to_string).collect(),
            uid,
        ));
        self.outcome
    }

    fn launch_as(&self, program: &str, uid: u32) -> Result<i64, Errno> {
        self.launches.borrow_mut().push((program.to_string(), uid));
        self.outcome.map(i64::from)
    }

    fn capture_as(
        &self,
        program: &str,
        argv: ElevateArgv<'_>,
        uid: u32,
        out: &mut [u8],
    ) -> Result<Captured, Errno> {
        self.captures.borrow_mut().push((
            program.to_string(),
            argv.iter().map(ToString::to_string).collect(),
            uid,
        ));
        let exit_code = self.outcome?;
        let Some(room) = out.get_mut(..self.printed.len()) else {
            return Ok(Captured::Overran { exit_code });
        };
        room.copy_from_slice(&self.printed);
        Ok(Captured::Output {
            exit_code,
            len: self.printed.len(),
        })
    }
}

/// An authenticator that refuses everything and records that it was
/// asked at all, so a test can prove a refusal happened *before* any
/// attempt was spent against the named account.
#[derive(Default)]
struct CountingAuth {
    attempts: core::cell::Cell<usize>,
}

impl Authenticator for CountingAuth {
    fn authenticate(&self, _credentials: &Credentials<'_>) -> Result<AuthenticatedUser, Errno> {
        self.attempts.set(self.attempts.get() + 1);
        Err(Errno::PermissionDenied)
    }

    fn authenticate_uid(&self, _uid: u32, _password: &str) -> Result<AuthenticatedUser, Errno> {
        self.attempts.set(self.attempts.get() + 1);
        Err(Errno::PermissionDenied)
    }
}

/// Sink counting events by id and keeping each record's textual fields.
#[derive(Default)]
struct CountSink {
    seen: RefCell<Vec<EventId>>,
    fields: RefCell<Vec<(alloc::string::String, alloc::string::String)>>,
}

impl CountSink {
    fn count(&self, id: EventId) -> usize {
        self.seen.borrow().iter().filter(|e| **e == id).count()
    }

    /// The value the most recent record carried under `key`.
    fn field(&self, key: &str) -> Option<alloc::string::String> {
        self.fields
            .borrow()
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, value)| value.clone())
    }
}

impl Sink for CountSink {
    fn write_event(&self, event: &Event<'_>) {
        self.seen.borrow_mut().push(event.id);
        for field in event.fields {
            if let tairix_log::FieldValue::Str(value) = field.value {
                self.fields
                    .borrow_mut()
                    .push((field.key.to_string(), value.to_string()));
            }
        }
    }
}

fn encoded_run(
    username: &str,
    password: &str,
    program: &str,
) -> ([u8; ELEVATE_MAX_REQUEST], usize) {
    encoded_run_with(username, password, program, &[])
}

fn encoded_run_with(
    username: &str,
    password: &str,
    program: &str,
    args: &[&str],
) -> ([u8; ELEVATE_MAX_REQUEST], usize) {
    let mut buf = [0u8; ELEVATE_MAX_REQUEST];
    let len = ElevateRequest::Run {
        username,
        password,
        program,
        argv: ElevateArgv::new(args).expect("within bounds"),
    }
    .encode(&mut buf)
    .expect("encodes");
    (buf, len)
}

fn encoded_launch(
    username: &str,
    password: &str,
    program: &str,
) -> ([u8; ELEVATE_MAX_REQUEST], usize) {
    let mut buf = [0u8; ELEVATE_MAX_REQUEST];
    let len = ElevateRequest::Launch {
        username,
        password,
        program,
    }
    .encode(&mut buf)
    .expect("encodes");
    (buf, len)
}

fn encoded_capture(
    username: &str,
    password: &str,
    program: &str,
    args: &[&str],
) -> ([u8; ELEVATE_MAX_REQUEST], usize) {
    let mut buf = [0u8; ELEVATE_MAX_REQUEST];
    let len = ElevateRequest::Capture {
        username,
        password,
        program,
        argv: ElevateArgv::new(args).expect("within bounds"),
    }
    .encode(&mut buf)
    .expect("encodes");
    (buf, len)
}

fn encoded_verify(password: &str) -> ([u8; ELEVATE_MAX_REQUEST], usize) {
    let mut buf = [0u8; ELEVATE_MAX_REQUEST];
    let len = ElevateRequest::Verify { password }
        .encode(&mut buf)
        .expect("encodes");
    (buf, len)
}

#[test]
fn correct_password_runs_the_program_as_the_account_and_audits() {
    let (buf, len) = encoded_run("root", "correct", "/System/Commands/users.app/Run");
    let launcher = MockLauncher::new(Ok(7));
    let sink = CountSink::default();
    let reply = decide(&buf[..len], 1, Some(0), &FixedAuth, &launcher, &sink);
    assert_eq!(reply, ElevateReply::Completed { exit_code: 7 });
    assert_eq!(
        launcher.runs.borrow().as_slice(),
        &[("/System/Commands/users.app/Run".to_string(), Vec::new(), 0)]
    );
    assert_eq!(sink.count(events::ELEVATE_GRANTED), 1);
    assert_eq!(sink.count(events::ELEVATE_REFUSED), 0);
}

#[test]
fn a_run_hands_its_argument_vector_to_the_launcher_verbatim() {
    let (buf, len) = encoded_run_with(
        "root",
        "correct",
        "/System/Commands/configure.app/Run",
        &["os.loginType", "text"],
    );
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let reply = decide(&buf[..len], 1, Some(0), &FixedAuth, &launcher, &sink);
    assert_eq!(reply, ElevateReply::Completed { exit_code: 0 });
    assert_eq!(
        launcher.runs.borrow().as_slice(),
        &[(
            "/System/Commands/configure.app/Run".to_string(),
            alloc::vec!["os.loginType".to_string(), "text".to_string()],
            0,
        )]
    );
}

#[test]
fn the_audit_records_the_argument_count_and_never_the_arguments() {
    // A tool that sets an account's password would take one on its
    // command line, and the broker cannot tell which argument that is —
    // so no argument reaches the log, only how many there were.
    let (buf, len) = encoded_run_with(
        "root",
        "correct",
        "/System/Commands/configure.app/Run",
        &["os.loginType", "hunter2"],
    );
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let _ = decide(&buf[..len], 1, Some(0), &FixedAuth, &launcher, &sink);
    assert_eq!(sink.count(events::ELEVATE_GRANTED), 1);
    assert_eq!(sink.field("args").as_deref(), Some("2"));
    assert!(!sink
        .fields
        .borrow()
        .iter()
        .any(|(_, value)| value.contains("hunter2") || value == "os.loginType"));
}

#[test]
fn an_over_long_argument_vector_is_refused_before_any_authentication() {
    // Hand-built past the bound the encoder enforces, so the refusal
    // proved here is the *decoder's* — the shape check the broker runs
    // before it spends an attempt against the named account.
    let mut buf = [0u8; ELEVATE_MAX_REQUEST];
    let mut at = 0;
    buf[at..at + 2].copy_from_slice(&1u16.to_le_bytes()); // ELEVATE_VERSION
    at += 2;
    buf[at] = 0; // the run opcode
    at += 1;
    for field in ["root", "correct", "/System/Commands/configure.app/Run"] {
        let len = u16::try_from(field.len()).expect("short");
        buf[at..at + 2].copy_from_slice(&len.to_le_bytes());
        at += 2;
        buf[at..at + field.len()].copy_from_slice(field.as_bytes());
        at += field.len();
    }
    let count = u16::try_from(ELEVATE_MAX_ARGS + 1).expect("small");
    buf[at..at + 2].copy_from_slice(&count.to_le_bytes());
    at += 2;

    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let auth = CountingAuth::default();
    let reply = decide(&buf[..at], 1, Some(0), &auth, &launcher, &sink);
    assert_eq!(reply, ElevateReply::Refused(Errno::LengthOutOfRange));
    assert_eq!(auth.attempts.get(), 0);
    assert!(launcher.runs.borrow().is_empty());
    assert_eq!(sink.count(events::ELEVATE_REFUSED), 1);
    assert_eq!(sink.field("cause").as_deref(), Some("malformed request"));
}

#[test]
fn wrong_password_and_unknown_account_are_refused_indistinguishably() {
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let (wrong, wrong_len) = encoded_run("root", "wrong", "/System/Commands/users.app/Run");
    let (unknown, unknown_len) =
        encoded_run("mallory", "correct", "/System/Commands/users.app/Run");
    let refused_wrong = decide(
        &wrong[..wrong_len],
        1,
        Some(0),
        &FixedAuth,
        &launcher,
        &sink,
    );
    let refused_unknown = decide(
        &unknown[..unknown_len],
        1,
        Some(0),
        &FixedAuth,
        &launcher,
        &sink,
    );
    // One reply for both causes: the requester learns nothing about
    // which part of the credentials failed.
    assert_eq!(
        refused_wrong,
        ElevateReply::Refused(Errno::PermissionDenied)
    );
    assert_eq!(refused_wrong, refused_unknown);
    assert!(launcher.runs.borrow().is_empty(), "nothing was spawned");
    assert_eq!(sink.count(events::ELEVATE_REFUSED), 2);
    assert_eq!(sink.count(events::ELEVATE_GRANTED), 0);
}

#[test]
fn a_caller_on_another_console_is_refused_before_parsing() {
    let (buf, len) = encoded_run("root", "correct", "/System/Commands/users.app/Run");
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let reply = decide(&buf[..len], 2, Some(0), &FixedAuth, &launcher, &sink);
    assert_eq!(reply, ElevateReply::Refused(Errno::PermissionDenied));
    assert!(launcher.runs.borrow().is_empty());
    assert_eq!(sink.count(events::ELEVATE_REFUSED), 1);
}

#[test]
fn a_malformed_request_is_refused_without_authentication() {
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let reply = decide(&[0xFF; 10], 1, Some(0), &FixedAuth, &launcher, &sink);
    assert!(matches!(reply.reply(), ElevateReply::Refused(_)));
    assert!(launcher.runs.borrow().is_empty());
    assert_eq!(sink.count(events::ELEVATE_REFUSED), 1);
}

#[test]
fn a_spawn_refusal_is_reported_verbatim_and_audited() {
    let (buf, len) = encoded_run("root", "correct", "/System/Commands/users.app/Run");
    let launcher = MockLauncher::new(Err(Errno::SignatureInvalid));
    let sink = CountSink::default();
    let reply = decide(&buf[..len], 1, Some(0), &FixedAuth, &launcher, &sink);
    assert_eq!(reply, ElevateReply::Refused(Errno::SignatureInvalid));
    assert_eq!(sink.count(events::ELEVATE_REFUSED), 1);
    assert_eq!(sink.count(events::ELEVATE_GRANTED), 0);
    assert_eq!(sink.field("cause").as_deref(), Some("launch failed"));
}

#[test]
fn correct_password_launches_the_program_without_waiting_and_audits() {
    let (buf, len) = encoded_launch("root", "correct", tairix_abi::DATETIME_RUN_PATH);
    let launcher = MockLauncher::new(Ok(4210));
    let sink = CountSink::default();
    let reply = decide(&buf[..len], 1, Some(0), &FixedAuth, &launcher, &sink);
    assert_eq!(reply, ElevateReply::Launched { pid: 4210 });
    assert_eq!(
        launcher.launches.borrow().as_slice(),
        &[(tairix_abi::DATETIME_RUN_PATH.to_string(), 0)]
    );
    assert!(
        launcher.runs.borrow().is_empty(),
        "a Launch request must never take the blocking run path"
    );
    assert_eq!(sink.count(events::LAUNCH_GRANTED), 1);
    assert_eq!(sink.count(events::ELEVATE_GRANTED), 0);
}

#[test]
fn a_launch_with_the_wrong_password_is_refused_and_starts_nothing() {
    let (buf, len) = encoded_launch("root", "wrong", tairix_abi::DATETIME_RUN_PATH);
    let launcher = MockLauncher::new(Ok(4210));
    let sink = CountSink::default();
    let reply = decide(&buf[..len], 1, Some(0), &FixedAuth, &launcher, &sink);
    assert_eq!(reply, ElevateReply::Refused(Errno::PermissionDenied));
    assert!(launcher.launches.borrow().is_empty());
    assert_eq!(sink.count(events::LAUNCH_REFUSED), 1);
    assert_eq!(sink.count(events::LAUNCH_GRANTED), 0);
}

#[test]
fn a_launch_spawn_refusal_is_reported_verbatim_and_audited() {
    let (buf, len) = encoded_launch("root", "correct", tairix_abi::DATETIME_RUN_PATH);
    let launcher = MockLauncher::new(Err(Errno::SignatureInvalid));
    let sink = CountSink::default();
    let reply = decide(&buf[..len], 1, Some(0), &FixedAuth, &launcher, &sink);
    assert_eq!(reply, ElevateReply::Refused(Errno::SignatureInvalid));
    assert_eq!(sink.count(events::LAUNCH_REFUSED), 1);
    assert_eq!(sink.count(events::LAUNCH_GRANTED), 0);
    assert_eq!(sink.field("cause").as_deref(), Some("launch failed"));
}

#[test]
fn a_launch_caller_on_another_console_is_refused_before_parsing() {
    let (buf, len) = encoded_launch("root", "correct", tairix_abi::DATETIME_RUN_PATH);
    let launcher = MockLauncher::new(Ok(4210));
    let sink = CountSink::default();
    let reply = decide(&buf[..len], 2, Some(0), &FixedAuth, &launcher, &sink);
    assert_eq!(reply, ElevateReply::Refused(Errno::PermissionDenied));
    assert!(launcher.launches.borrow().is_empty());
    assert_eq!(sink.count(events::ELEVATE_REFUSED), 1);
    assert_eq!(sink.count(events::LAUNCH_REFUSED), 0);
}

#[test]
fn a_launched_program_that_ended_abnormally_is_audited_once() {
    let sink = CountSink::default();
    audit_launch_ended_abnormally(&sink, 4210, 83);
    assert_eq!(sink.count(events::LAUNCH_ENDED_ABNORMALLY), 1);
    assert_eq!(sink.field("pid").as_deref(), Some("4210"));
    assert_eq!(sink.field("status").as_deref(), Some("83"));
}

#[test]
fn a_launched_program_that_exited_cleanly_is_not_audited() {
    let sink = CountSink::default();
    audit_launch_ended_abnormally(&sink, 4210, 0);
    assert_eq!(sink.count(events::LAUNCH_ENDED_ABNORMALLY), 0);
}

#[test]
fn a_reserved_load_failure_status_is_named_in_words() {
    let sink = CountSink::default();
    audit_launch_ended_abnormally(&sink, 4210, LOAD_UNVERIFIED);
    assert_eq!(sink.count(events::LAUNCH_ENDED_ABNORMALLY), 1);
    assert_eq!(
        sink.field("reason").as_deref(),
        tairix_abi::load_failure_reason(LOAD_UNVERIFIED)
    );
}

#[test]
fn a_status_outside_the_load_band_states_the_code_and_invents_no_reason() {
    let sink = CountSink::default();
    audit_launch_ended_abnormally(&sink, 4210, 7);
    assert!(tairix_abi::load_failure_reason(7).is_none());
    assert_eq!(sink.field("status").as_deref(), Some("7"));
    assert_eq!(sink.field("reason").as_deref(), Some("-"));
}

#[test]
fn a_capture_returns_what_the_run_printed_and_audits_the_volume_only() {
    let (buf, len) = encoded_capture(
        "root",
        "correct",
        "/System/Commands/configure.app/Run",
        &["wan.ipv4.address"],
    );
    let launcher = MockLauncher::printing(Ok(0), b"10.0.0.7/24\n");
    let sink = CountSink::default();
    let reply = decide(&buf[..len], 1, Some(0), &FixedAuth, &launcher, &sink);
    assert_eq!(
        reply,
        ElevateReply::Captured {
            exit_code: 0,
            output: b"10.0.0.7/24\n",
        }
    );
    assert_eq!(
        launcher.captures.borrow().as_slice(),
        &[(
            "/System/Commands/configure.app/Run".to_string(),
            alloc::vec!["wan.ipv4.address".to_string()],
            0,
        )]
    );
    assert!(
        launcher.runs.borrow().is_empty(),
        "a Capture request must never take the console-inheriting run path"
    );
    assert_eq!(sink.count(events::CAPTURE_GRANTED), 1);
    assert_eq!(sink.count(events::ELEVATE_GRANTED), 0);
    assert_eq!(sink.field("bytes").as_deref(), Some("12"));
    assert_eq!(sink.field("output").as_deref(), Some("returned"));
    // The relayed text itself never reaches the log: the broker hands
    // it over without reading it, so it cannot know what is a secret.
    assert!(!sink
        .fields
        .borrow()
        .iter()
        .any(|(_, value)| value.contains("10.0.0.7")));
}

#[test]
fn a_run_that_prints_past_the_bound_returns_no_bytes_at_all() {
    let (buf, len) = encoded_capture("root", "correct", "/System/Commands/cat.app/Run", &[]);
    let flood = alloc::vec![b'x'; ELEVATE_MAX_OUTPUT + 1];
    let launcher = MockLauncher::printing(Ok(3), &flood);
    let sink = CountSink::default();
    let reply = decide(&buf[..len], 1, Some(0), &FixedAuth, &launcher, &sink);
    // The run happened and its code is real; a prefix a caller could
    // mistake for the whole is not offered.
    assert_eq!(reply, ElevateReply::Overran { exit_code: 3 });
    assert_eq!(sink.count(events::CAPTURE_GRANTED), 1);
    assert_eq!(sink.field("output").as_deref(), Some("overran"));
    assert_eq!(sink.field("bytes").as_deref(), Some("0"));
}

#[test]
fn a_capture_with_the_wrong_password_is_refused_and_runs_nothing() {
    let (buf, len) = encoded_capture("root", "wrong", "/System/Commands/configure.app/Run", &[]);
    let launcher = MockLauncher::printing(Ok(0), b"secret\n");
    let sink = CountSink::default();
    let reply = decide(&buf[..len], 1, Some(0), &FixedAuth, &launcher, &sink);
    assert_eq!(reply, ElevateReply::Refused(Errno::PermissionDenied));
    assert!(launcher.captures.borrow().is_empty());
    assert_eq!(sink.count(events::CAPTURE_REFUSED), 1);
    assert_eq!(sink.count(events::CAPTURE_GRANTED), 0);
}

#[test]
fn a_capture_spawn_refusal_is_reported_verbatim_and_audited() {
    let (buf, len) = encoded_capture("root", "correct", "/System/Commands/cat.app/Run", &[]);
    let launcher = MockLauncher::new(Err(Errno::SignatureInvalid));
    let sink = CountSink::default();
    let reply = decide(&buf[..len], 1, Some(0), &FixedAuth, &launcher, &sink);
    assert_eq!(reply, ElevateReply::Refused(Errno::SignatureInvalid));
    assert_eq!(sink.count(events::CAPTURE_REFUSED), 1);
    assert_eq!(sink.count(events::CAPTURE_GRANTED), 0);
    assert_eq!(sink.field("cause").as_deref(), Some("run failed"));
}

#[test]
fn a_capture_caller_on_another_console_is_refused_before_parsing() {
    let (buf, len) = encoded_capture("root", "correct", "/System/Commands/configure.app/Run", &[]);
    let launcher = MockLauncher::printing(Ok(0), b"secret\n");
    let sink = CountSink::default();
    let reply = decide(&buf[..len], 2, Some(0), &FixedAuth, &launcher, &sink);
    assert_eq!(reply, ElevateReply::Refused(Errno::PermissionDenied));
    assert!(launcher.captures.borrow().is_empty());
    assert_eq!(sink.count(events::ELEVATE_REFUSED), 1);
    assert_eq!(sink.count(events::CAPTURE_REFUSED), 0);
}

#[test]
fn a_capture_that_printed_nothing_answers_an_empty_output_not_a_refusal() {
    let (buf, len) = encoded_capture("root", "correct", "/System/Commands/configure.app/Run", &[]);
    let launcher = MockLauncher::printing(Ok(0), b"");
    let sink = CountSink::default();
    let reply = decide(&buf[..len], 1, Some(0), &FixedAuth, &launcher, &sink);
    assert_eq!(
        reply,
        ElevateReply::Captured {
            exit_code: 0,
            output: b"",
        }
    );
    assert_eq!(sink.field("bytes").as_deref(), Some("0"));
    assert_eq!(sink.field("output").as_deref(), Some("returned"));
}

/// A reader answering `chunks` in order, then end of stream.
fn scripted<'a>(chunks: &'a [&'a [u8]]) -> impl FnMut(&mut [u8]) -> Result<usize, Errno> + use<'a> {
    let mut left: Vec<&[u8]> = chunks.to_vec();
    left.reverse();
    move |buf: &mut [u8]| {
        let Some(chunk) = left.pop() else {
            return Ok(0);
        };
        let take = chunk.len().min(buf.len());
        buf[..take].copy_from_slice(&chunk[..take]);
        // A short read leaves the rest for the next call, exactly as a
        // pipe whose buffer holds less than the reader asked for does.
        if take < chunk.len() {
            left.push(&chunk[take..]);
        }
        Ok(take)
    }
}

#[test]
fn a_drain_keeps_the_whole_stream_when_it_fits() {
    let mut out = [0u8; 8];
    assert_eq!(
        drain_bounded(&mut out, scripted(&[b"ab", b"cd", b"e"])),
        Ok(Some(5))
    );
    assert_eq!(&out[..5], b"abcde");
}

#[test]
fn a_stream_that_exactly_fills_the_buffer_is_not_an_overrun() {
    // The edge the bound turns on: `out` being full says nothing about
    // whether more is coming, so the verdict waits on a further read.
    let mut out = [0u8; 4];
    assert_eq!(drain_bounded(&mut out, scripted(&[b"abcd"])), Ok(Some(4)));
    assert_eq!(&out, b"abcd");
}

#[test]
fn one_byte_past_the_buffer_returns_nothing_at_all() {
    let mut out = [0u8; 4];
    assert_eq!(drain_bounded(&mut out, scripted(&[b"abcde"])), Ok(None));
}

#[test]
fn a_drain_reads_to_end_of_stream_even_after_it_has_overrun() {
    // A writer blocked on a full pipe never exits, so the reap that
    // follows would hang if the drain stopped at the bound.
    let mut reads = 0usize;
    let mut out = [0u8; 2];
    let drained = drain_bounded(&mut out, |buf| {
        reads += 1;
        if reads > 6 {
            return Ok(0);
        }
        let take = buf.len().min(4);
        buf[..take].fill(b'x');
        Ok(take)
    });
    assert_eq!(drained, Ok(None));
    assert_eq!(reads, 7, "every read to end of stream, then the zero");
}

#[test]
fn an_empty_stream_and_an_empty_buffer_both_answer_nothing_kept() {
    let mut out = [0u8; 4];
    assert_eq!(drain_bounded(&mut out, scripted(&[])), Ok(Some(0)));
    let mut none = [0u8; 0];
    assert_eq!(drain_bounded(&mut none, scripted(&[])), Ok(Some(0)));
    // A buffer with no room at all still cannot keep a byte.
    let mut none = [0u8; 0];
    assert_eq!(drain_bounded(&mut none, scripted(&[b"a"])), Ok(None));
}

#[test]
fn a_read_failure_surfaces_verbatim_and_reports_nothing_partial() {
    let mut out = [0u8; 4];
    assert_eq!(
        drain_bounded(&mut out, |_| Err(Errno::BrokenPipe)),
        Err(Errno::BrokenPipe)
    );
}

#[test]
fn verify_with_the_right_password_answers_verified_and_runs_nothing() {
    let (buf, len) = encoded_verify("correct");
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let reply = decide(&buf[..len], 1, Some(0), &FixedAuth, &launcher, &sink);
    assert_eq!(reply, ElevateReply::Verified);
    assert!(
        launcher.runs.borrow().is_empty(),
        "a Verify request must never invoke the launcher"
    );
    assert_eq!(sink.count(events::VERIFY_GRANTED), 1);
    assert_eq!(sink.count(events::VERIFY_REFUSED), 0);
}

#[test]
fn verify_with_the_wrong_password_is_refused_and_runs_nothing() {
    let (buf, len) = encoded_verify("wrong");
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let reply = decide(&buf[..len], 1, Some(0), &FixedAuth, &launcher, &sink);
    assert_eq!(reply, ElevateReply::Refused(Errno::PermissionDenied));
    assert!(launcher.runs.borrow().is_empty());
    assert_eq!(sink.count(events::VERIFY_REFUSED), 1);
    assert_eq!(sink.count(events::VERIFY_GRANTED), 0);
}

#[test]
fn verify_with_an_attested_uid_owning_no_account_is_indistinguishable_from_wrong_password() {
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let (unowned, unowned_len) = encoded_verify("correct");
    let (wrong, wrong_len) = encoded_verify("wrong");
    let no_account = decide(
        &unowned[..unowned_len],
        1,
        Some(9999),
        &FixedAuth,
        &launcher,
        &sink,
    );
    let wrong_password = decide(
        &wrong[..wrong_len],
        1,
        Some(0),
        &FixedAuth,
        &launcher,
        &sink,
    );
    // Both take the identical `authenticate_uid` path and answer the
    // identical refusal — an unresolvable uid never runs any faster or
    // differently than a wrong password on a real one.
    assert_eq!(no_account, ElevateReply::Refused(Errno::PermissionDenied));
    assert_eq!(no_account, wrong_password);
    assert!(launcher.runs.borrow().is_empty());
    assert_eq!(sink.count(events::VERIFY_REFUSED), 2);
}

#[test]
fn verify_without_an_attested_uid_is_refused_before_authenticating() {
    let (buf, len) = encoded_verify("correct");
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let reply = decide(&buf[..len], 1, None, &FixedAuth, &launcher, &sink);
    assert_eq!(reply, ElevateReply::Refused(Errno::PermissionDenied));
    assert!(launcher.runs.borrow().is_empty());
    assert_eq!(sink.count(events::VERIFY_REFUSED), 1);
    assert_eq!(sink.count(events::VERIFY_GRANTED), 0);
}

#[test]
fn a_verify_caller_on_another_console_is_refused_before_parsing() {
    let (buf, len) = encoded_verify("correct");
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let reply = decide(&buf[..len], 2, Some(0), &FixedAuth, &launcher, &sink);
    assert_eq!(reply, ElevateReply::Refused(Errno::PermissionDenied));
    assert!(launcher.runs.borrow().is_empty());
    assert_eq!(sink.count(events::ELEVATE_REFUSED), 1);
    assert_eq!(sink.count(events::VERIFY_REFUSED), 0);
}

/// Each refusal a fresh account can collect before its meter cools it.
fn spend_free_attempts(bench: &Bench<'_>, bytes: &[u8]) {
    for _ in 0..=FREE_ATTEMPTS {
        let _ = bench.decide(bytes, shell(OWN_CONSOLE, Some(1000)));
    }
}

#[test]
fn an_account_whose_ceiling_covers_the_program_runs_it() {
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let mut bench = Bench::new(&FixedAuth, &launcher, &sink);
    bench.programs = Manifests::shipped().with(
        "/System/Commands/users.app/Run",
        &[CapabilityId::USER_ADMIN],
    );
    let (buf, len) = encoded_run("root", "correct", "/System/Commands/users.app/Run");
    let reply = bench.decide(&buf[..len], shell(OWN_CONSOLE, Some(1000)));
    assert_eq!(reply, ElevateReply::Completed { exit_code: 0 });
    assert_eq!(launcher.runs.borrow().len(), 1);
}

#[test]
fn an_account_that_may_not_allow_the_program_is_refused_as_a_wrong_password_is() {
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let mut bench = Bench::new(&FixedAuth, &launcher, &sink);
    bench.programs = Manifests::shipped().with(
        "/System/Commands/users.app/Run",
        &[CapabilityId::USER_ADMIN, CapabilityId::DRV_KERNEL],
    );
    let (right, right_len) = encoded_run("root", "correct", "/System/Commands/users.app/Run");
    let (wrong, wrong_len) = encoded_run("root", "wrong", "/System/Commands/users.app/Run");
    let not_allowed = bench.decide(&right[..right_len], shell(OWN_CONSOLE, Some(1000)));
    assert_eq!(
        sink.field("cause").as_deref(),
        Some("account may not allow the program")
    );
    let wrong_password = bench.decide(&wrong[..wrong_len], shell(OWN_CONSOLE, Some(1000)));
    assert_eq!(not_allowed, ElevateReply::Refused(Errno::PermissionDenied));
    assert_eq!(not_allowed, wrong_password);
    assert!(launcher.runs.borrow().is_empty(), "nothing ran");
    assert_eq!(sink.count(events::ELEVATE_REFUSED), 2);
}

#[test]
fn a_launch_and_a_capture_are_held_to_the_same_authority() {
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let mut bench = Bench::new(&FixedAuth, &launcher, &sink);
    bench.programs = Manifests::shipped()
        .with(tairix_abi::DATETIME_RUN_PATH, &[CapabilityId::DRV_KERNEL])
        .with("/System/Commands/cat.app/Run", &[CapabilityId::DRV_KERNEL]);
    let (launch, launch_len) = encoded_launch("root", "correct", tairix_abi::DATETIME_RUN_PATH);
    let (capture, capture_len) =
        encoded_capture("root", "correct", "/System/Commands/cat.app/Run", &[]);
    let denied = ElevateReply::Refused(Errno::PermissionDenied);
    assert_eq!(
        bench.decide(&launch[..launch_len], shell(OWN_CONSOLE, Some(1000))),
        denied
    );
    assert_eq!(
        bench.decide(&capture[..capture_len], shell(OWN_CONSOLE, Some(1000))),
        denied
    );
    assert!(launcher.launches.borrow().is_empty());
    assert!(launcher.captures.borrow().is_empty());
    assert_eq!(sink.count(events::LAUNCH_REFUSED), 1);
    assert_eq!(sink.count(events::CAPTURE_REFUSED), 1);
}

#[test]
fn a_program_with_no_manifest_to_read_is_refused_before_any_attempt() {
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let auth = CountingAuth::default();
    let (buf, len) = encoded_run("root", "correct", "/missing");
    let reply = decide(&buf[..len], 1, Some(1000), &auth, &launcher, &sink);
    assert_eq!(reply, ElevateReply::Refused(Errno::NotFound));
    assert_eq!(auth.attempts.get(), 0);
    assert_eq!(sink.field("cause").as_deref(), Some("manifest unreadable"));
}

#[test]
fn guesses_past_the_free_attempts_are_answered_cooling_and_never_checked() {
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let auth = CountingAuth::default();
    let bench = Bench::new(&auth, &launcher, &sink);
    let (wrong, wrong_len) = encoded_run("root", "wrong", "/System/Commands/users.app/Run");
    spend_free_attempts(&bench, &wrong[..wrong_len]);
    let checked = auth.attempts.get();
    let reply = bench.decide(&wrong[..wrong_len], shell(OWN_CONSOLE, Some(1000)));
    assert!(
        matches!(reply.reply(), ElevateReply::Cooling { retry_after } if retry_after > Duration64::ZERO),
        "{reply:?}"
    );
    assert_eq!(
        auth.attempts.get(),
        checked,
        "a cooling account is not checked"
    );
    assert_eq!(sink.field("cause").as_deref(), Some("cooling down"));
    assert!(sink.field("retry_after_s").is_some());
}

#[test]
fn a_cooling_account_is_not_checked_even_with_the_right_password() {
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let bench = Bench::new(&FixedAuth, &launcher, &sink);
    let (wrong, wrong_len) = encoded_run("root", "wrong", "/System/Commands/users.app/Run");
    let (right, right_len) = encoded_run("root", "correct", "/System/Commands/users.app/Run");
    spend_free_attempts(&bench, &wrong[..wrong_len]);
    let reply = bench.decide(&right[..right_len], shell(OWN_CONSOLE, Some(1000)));
    assert!(
        matches!(reply.reply(), ElevateReply::Cooling { .. }),
        "{reply:?}"
    );
    assert!(launcher.runs.borrow().is_empty());
    // Once the cooldown has passed, the same password is checked again.
    bench.now.set(Duration64::from_secs(3600));
    let reply = bench.decide(&right[..right_len], shell(OWN_CONSOLE, Some(1000)));
    assert_eq!(reply, ElevateReply::Completed { exit_code: 0 });
}

#[test]
fn an_account_refused_its_authority_is_charged_as_a_wrong_password_is() {
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let mut bench = Bench::new(&FixedAuth, &launcher, &sink);
    bench.programs = Manifests::shipped().with(
        "/System/Commands/users.app/Run",
        &[CapabilityId::DRV_KERNEL],
    );
    let (right, right_len) = encoded_run("root", "correct", "/System/Commands/users.app/Run");
    spend_free_attempts(&bench, &right[..right_len]);
    let reply = bench.decide(&right[..right_len], shell(OWN_CONSOLE, Some(1000)));
    assert!(
        matches!(reply.reply(), ElevateReply::Cooling { .. }),
        "the meter cannot tell this caller its password was right: {reply:?}"
    );
}

#[test]
fn verifying_ones_own_password_is_metered_with_every_named_form() {
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let bench = Bench::new(&FixedAuth, &launcher, &sink);
    let (wrong, wrong_len) = encoded_verify("wrong");
    for _ in 0..=FREE_ATTEMPTS {
        let _ = bench.decide(&wrong[..wrong_len], shell(OWN_CONSOLE, Some(0)));
    }
    let (verify, verify_len) = encoded_verify("correct");
    let (run, run_len) = encoded_run("root", "correct", "/System/Commands/users.app/Run");
    for (bytes, form) in [(&verify[..verify_len], "verify"), (&run[..run_len], "run")] {
        let reply = bench.decide(bytes, shell(OWN_CONSOLE, Some(0)));
        assert!(
            matches!(reply.reply(), ElevateReply::Cooling { .. }),
            "a {form} for the account the guesses were against: {reply:?}"
        );
    }
    assert_eq!(sink.count(events::VERIFY_GRANTED), 0);
}

#[test]
fn a_right_password_clears_the_accounts_meter() {
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let bench = Bench::new(&FixedAuth, &launcher, &sink);
    let (wrong, wrong_len) = encoded_run("root", "wrong", "/System/Commands/users.app/Run");
    let (right, right_len) = encoded_run("root", "correct", "/System/Commands/users.app/Run");
    for _ in 0..FREE_ATTEMPTS {
        let _ = bench.decide(&wrong[..wrong_len], shell(OWN_CONSOLE, Some(1000)));
    }
    let reply = bench.decide(&right[..right_len], shell(OWN_CONSOLE, Some(1000)));
    assert_eq!(reply, ElevateReply::Completed { exit_code: 0 });
    // The account starts again from its free attempts.
    for _ in 0..FREE_ATTEMPTS {
        let reply = bench.decide(&wrong[..wrong_len], shell(OWN_CONSOLE, Some(1000)));
        assert_eq!(reply, ElevateReply::Refused(Errno::PermissionDenied));
    }
}

fn encoded_authorisers(program: &str, offset: u32) -> ([u8; ELEVATE_MAX_REQUEST], usize) {
    let mut buf = [0u8; ELEVATE_MAX_REQUEST];
    let len = ElevateRequest::Authorisers { program, offset }
        .encode(&mut buf)
        .expect("encodes");
    (buf, len)
}

#[test]
fn the_presenting_desktop_is_told_who_may_allow_a_system_program() {
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let mut bench = Bench::new(&FixedAuth, &launcher, &sink);
    bench.programs =
        Manifests::shipped().with(tairix_abi::DATETIME_RUN_PATH, &[CapabilityId::TIME_SET]);
    bench.directory.presenting = Some(1000);
    let desktop = identity(DESKTOP_BUNDLE, SYSTEM);
    let caller = Caller {
        console: OWN_CONSOLE,
        uid: Some(1000),
        app: Some(&desktop),
    };
    let (buf, len) = encoded_authorisers(tairix_abi::DATETIME_RUN_PATH, 0);
    let decided = bench.decide(&buf[..len], caller);
    let ElevateReply::Authorisers { own, page } = decided.reply() else {
        panic!("not a listing: {decided:?}");
    };
    assert!(own);
    let listing = decode_account_page(page).expect("a page the session reads");
    assert_eq!(listing.total(), 1);
    assert_eq!(
        bench.directory.asked.borrow().as_slice(),
        &[(caps(&[CapabilityId::TIME_SET]), 1000, 0)],
        "asked with the program's own request, for the caller, from the offset given"
    );
    assert_eq!(sink.count(events::AUTHORISERS_SENT), 1);
    assert_eq!(sink.field("total").as_deref(), Some("1"));
}

#[test]
fn nobody_but_the_presenting_desktop_under_the_systems_publisher_is_told() {
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let mut bench = Bench::new(&FixedAuth, &launcher, &sink);
    bench.directory.presenting = Some(1000);
    let desktop = identity(DESKTOP_BUNDLE, SYSTEM);
    let impostor = identity(DESKTOP_BUNDLE, PublisherId::from_raw([0x66; 32]));
    let settings = identity("os.tairix.settings", SYSTEM);
    let (buf, len) = encoded_authorisers(tairix_abi::DATETIME_RUN_PATH, 0);
    for (what, caller) in [
        (
            "another console",
            Caller {
                console: 2,
                uid: Some(1000),
                app: Some(&desktop),
            },
        ),
        (
            "a session behind the seat",
            Caller {
                console: OWN_CONSOLE,
                uid: Some(1001),
                app: Some(&desktop),
            },
        ),
        (
            "no attested uid",
            Caller {
                console: OWN_CONSOLE,
                uid: None,
                app: Some(&desktop),
            },
        ),
        (
            "no bundle",
            Caller {
                console: OWN_CONSOLE,
                uid: Some(1000),
                app: None,
            },
        ),
        (
            "another developer's bundle",
            Caller {
                console: OWN_CONSOLE,
                uid: Some(1000),
                app: Some(&impostor),
            },
        ),
        (
            "another system program",
            Caller {
                console: OWN_CONSOLE,
                uid: Some(1000),
                app: Some(&settings),
            },
        ),
    ] {
        let reply = bench.decide(&buf[..len], caller);
        assert_eq!(
            reply,
            ElevateReply::Refused(Errno::PermissionDenied),
            "{what}"
        );
    }
    bench.own_app = None;
    let reply = bench.decide(
        &buf[..len],
        Caller {
            console: OWN_CONSOLE,
            uid: Some(1000),
            app: Some(&desktop),
        },
    );
    assert_eq!(
        reply,
        ElevateReply::Refused(Errno::PermissionDenied),
        "an authority with no publisher of its own recognises no desktop"
    );
    assert!(
        bench.directory.asked.borrow().is_empty(),
        "no account was looked at"
    );
    assert_eq!(sink.count(events::AUTHORISERS_SENT), 0);
}

#[test]
fn only_a_program_in_a_system_store_is_listed_for() {
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let mut bench = Bench::new(&FixedAuth, &launcher, &sink);
    bench.directory.presenting = Some(1000);
    bench.programs = Manifests::shipped()
        .with(
            "/Users/ada/Commands/probe.app/Run",
            &[CapabilityId::DRV_KERNEL],
        )
        .with(
            "/System/Commands/../../Users/ada/Commands/probe.app/Run",
            &[CapabilityId::DRV_KERNEL],
        );
    let desktop = identity(DESKTOP_BUNDLE, SYSTEM);
    let caller = Caller {
        console: OWN_CONSOLE,
        uid: Some(1000),
        app: Some(&desktop),
    };
    for program in [
        "/Users/ada/Commands/probe.app/Run",
        "/System/Commands/../../Users/ada/Commands/probe.app/Run",
        "/System/Commands/users.app",
    ] {
        let (buf, len) = encoded_authorisers(program, 0);
        let reply = bench.decide(&buf[..len], caller);
        assert_eq!(
            reply,
            ElevateReply::Refused(Errno::PermissionDenied),
            "{program}"
        );
    }
    assert!(bench.directory.asked.borrow().is_empty());
    assert_eq!(sink.field("cause").as_deref(), Some("not a system program"));
}

#[test]
fn a_system_program_whose_manifest_cannot_be_read_is_refused_with_its_code() {
    let launcher = MockLauncher::new(Ok(0));
    let sink = CountSink::default();
    let mut bench = Bench::new(&FixedAuth, &launcher, &sink);
    bench.directory.presenting = Some(1000);
    let desktop = identity(DESKTOP_BUNDLE, SYSTEM);
    let caller = Caller {
        console: OWN_CONSOLE,
        uid: Some(1000),
        app: Some(&desktop),
    };
    let (buf, len) = encoded_authorisers("/System/Commands/absent.app/Run", 3);
    assert_eq!(
        bench.decide(&buf[..len], caller),
        ElevateReply::Refused(Errno::NotFound)
    );
    assert!(bench.directory.asked.borrow().is_empty());
    assert_eq!(sink.count(events::AUTHORISERS_REFUSED), 1);
}
