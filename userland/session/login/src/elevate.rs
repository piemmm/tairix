//! The per-invocation elevation broker (`plans/CAPABILITY_USE.md` CU5).
//!
//! While a session runs, login serves its console's elevation call endpoint
//! ([`tairix_abi::elevate`]): the session's shell forwards an
//! `elevate <user> <program>` request, and this module decides it —
//! re-authenticating the target account through the **same**
//! [`Authenticator`] the login prompt uses (timing-equalised, refusals
//! indistinguishable), then running the program as that account through the
//! injected [`ElevateLauncher`] and reporting its exit code. The requesting
//! shell's own identity and capability set are never touched: the elevated
//! child's set is derived kernel-side as
//! `its manifest ∩ the target account's ceiling`, exactly as at login.
//!
//! A graphical caller cannot use that exchange — its reply arrives only
//! once the elevated program has exited, so a desktop session posting it
//! would stop serving windows to the very program it is waiting for, and
//! the two would deadlock. Such a caller posts
//! [`ElevateRequest::Launch`], which takes the identical
//! re-authentication path and answers the started program's pid instead.
//! The program is *login's* child either way, so the `Run` binary reaps a
//! launched one on its own loop rather than leaving it a zombie, handing an
//! abnormal exit to [`audit_launch_ended_abnormally`] — such a child's
//! `stderr` is login's console, invisible behind a desktop, so the reaper is
//! the only component that can state how it ended.
//!
//! A caller that must *show* what an elevated run printed posts
//! [`ElevateRequest::Capture`]. It takes the identical re-authentication
//! and runs the program on the identical terms, but its standard output is
//! bound to a pipe the `Run` binary owns instead of login's console, and
//! the reply carries the drained bytes. The relay is bounded
//! ([`tairix_abi::elevate::ELEVATE_MAX_OUTPUT`]) and a run that prints more
//! is answered [`ElevateReply::Overran`] with no bytes at all, so a caller
//! never reads a prefix as the whole. The audit records how much was
//! relayed and never what it was.
//!
//! The same endpoint also answers a narrower [`ElevateRequest::Verify`]
//! request: re-authenticate the **caller's own** kernel-attested account and
//! run nothing. This is the primitive a graphical session's screen lock
//! needs, reusing every part of the broker — the same timing-equalised
//! authenticator, the same indistinguishable refusal, the same per-attempt
//! audit — rather than a second authenticator existing anywhere in the tree.
//! It is strictly weaker than a `Run` request (it never spawns a program)
//! and narrower still (it can only ever check the caller's *own* account,
//! never one it names), so it grants no authority a `Run` request did not
//! already carry.
//!
//! An account may allow a program only when everything the program's signed
//! manifest requests ([`ProgramRequests`]) lies within the account's ceiling.
//! One that may not is refused, and metered, exactly as a wrong password is,
//! so the reply never says the password was right. Every attempt in every
//! form is metered per account by the login screen's own [`AttemptBudget`],
//! so guesses cannot be spread across surfaces. Which accounts may allow a
//! system program is told to one caller, the desktop session presenting on
//! this console ([`ElevateRequest::Authorisers`]), so its prompt offers only
//! accounts that can succeed.
//!
//! Like the [`Login`](crate::Login) state machine this is pure decision
//! logic over injected seams, so every branch is host-tested; the `Run`
//! binary owns the IPC serve loop (`call_recv` → this → `call_reply`) and
//! the syscall-backed launcher.

use alloc::vec::Vec;

use tairix_abi::elevate::{ElevateArgv, ElevateReply, ElevateRequest};
use tairix_abi::session_ipc::{SessionAccount, DESKTOP_BUNDLE};
use tairix_abi::time::Duration64;
use tairix_abi::{AppIdentity, Errno};
use tairix_caps::CapabilitySet;
use tairix_log::{log, Event, EventId, Field, Level, Sink};

use crate::broker::encode_listing;
use crate::budget::AttemptBudget;
use crate::decfmt::DecBuf;
use crate::events;
use crate::session::{AuthenticatedUser, Authenticator, Credentials};

/// Runs a re-authenticated elevation command: spawns `program` as `uid` on
/// login's own console and blocks until it exits.
///
/// The `Run` binary backs this with the `spawn` syscall's spawn-as-user
/// switch (login's `CAP_SPAWN_AS_USER`) plus `wait`; tests use in-memory
/// fixtures. Separate from [`crate::SessionLauncher`], which starts a
/// *session* from an [`crate::AuthenticatedUser`] record's shell of choice —
/// an elevation runs one explicit program and must never consult the
/// account's shell.
pub trait ElevateLauncher {
    /// Spawn `program` as `uid` with the argument vector `argv`, block until
    /// it exits, and return its exit code.
    ///
    /// The arguments are data the implementation hands the child verbatim;
    /// they confer nothing, and the child's capability set is still its
    /// manifest intersected with the target account's ceiling.
    ///
    /// # Errors
    ///
    /// Returns the implementation's [`Errno`] verbatim when the program
    /// cannot be spawned or reaped (unknown path, spawn refused, …); the
    /// broker reports it to the requester and audits it.
    fn run_as(&self, program: &str, argv: ElevateArgv<'_>, uid: u32) -> Result<i32, Errno>;

    /// Spawn `program` as `uid` and return its pid **without waiting for
    /// it**.
    ///
    /// Serves a caller that must keep running while the elevated program
    /// does — a graphical session, which owns the compositor that program
    /// draws through, and would deadlock on a reply that waits for its
    /// exit. The started process is the implementation's own child, so the
    /// implementation owns reaping it; the requester never can.
    ///
    /// # Errors
    ///
    /// Returns the implementation's [`Errno`] verbatim when the program
    /// cannot be spawned.
    fn launch_as(&self, program: &str, uid: u32) -> Result<i64, Errno>;

    /// Spawn `program` as `uid` with its standard output bound to a
    /// collector the implementation owns, drain that output into `out`,
    /// block until the program exits, and report what it produced.
    ///
    /// The same authority and the same terms as [`Self::run_as`]; only the
    /// destination of the child's standard output differs. The
    /// implementation drains to end-of-stream **before** reaping, because a
    /// child that fills the collector blocks until it is emptied and a
    /// reaper that waited first would never empty it.
    ///
    /// # Errors
    ///
    /// Returns the implementation's [`Errno`] verbatim when the program
    /// cannot be spawned, collected, or reaped.
    fn capture_as(
        &self,
        program: &str,
        argv: ElevateArgv<'_>,
        uid: u32,
        out: &mut [u8],
    ) -> Result<Captured, Errno>;
}

/// What one [`ElevateLauncher::capture_as`] run produced.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Captured {
    /// The run's exit code, and how many leading bytes of the caller's
    /// buffer its whole standard output filled.
    Output {
        /// The program's exit status, exactly as `wait` reported it.
        exit_code: i32,
        /// Bytes of the caller's buffer the output filled.
        len: usize,
    },
    /// The run's exit code; it printed more than the caller's buffer holds,
    /// so none of it is offered.
    ///
    /// The run still happened — whatever it did, it did — which is why this
    /// is an outcome rather than an error, and why no prefix is handed back
    /// to be mistaken for the whole.
    Overran {
        /// The program's exit status.
        exit_code: i32,
    },
}

/// What a program's signed manifest requests, as the authority reads it.
///
/// The `Run` binary reads the bundle's `AppInfo` under login's own identity;
/// tests script it. A manifest that changes between this read and the spawn
/// cannot widen anything: the kernel still grants the program only what its
/// verified manifest requests within the account's ceiling.
pub trait ProgramRequests {
    /// The capabilities requested by the manifest of the bundle whose entry
    /// `program` is.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] when `program` is no bundle's entry or its bundle
    /// carries no manifest; the read's or the decode's own refusal otherwise.
    fn requested(&self, program: &str) -> Result<CapabilitySet, Errno>;
}

/// The broker's view of the machine's accounts and of the session presenting
/// on its seat.
///
/// The production implementation is [`crate::DbAccounts`]. It reports a
/// ceiling only as a yes or no against a program's request, and an account
/// only as the tile a prompt draws.
pub trait ElevateDirectory {
    /// The login name of the account `uid` owns: what its attempts are
    /// metered under.
    fn login_name(&self, uid: u32) -> Option<&str>;

    /// The uid owning the session presenting on the seat, if one does.
    fn presenting(&self) -> Option<u32>;

    /// The login-able accounts whose ceilings cover `requested`, from the
    /// `offset`th, the account `caller` owns first when it is one of them.
    fn authorisers(&self, requested: &CapabilitySet, caller: u32, offset: u32) -> AuthoriserPage;
}

/// One page of [`ElevateDirectory::authorisers`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthoriserPage {
    /// Whether the caller's own account is one of them.
    pub own: bool,
    /// How many there are in all.
    pub total: u32,
    /// Those from the requested offset, at most a page of them.
    pub accounts: Vec<SessionAccount>,
}

/// Who posted one request, as the kernel attests it — never as the request
/// claims.
#[derive(Copy, Clone, Debug)]
pub struct Caller<'a> {
    /// The console its standard streams are on.
    pub console: u64,
    /// Its uid, [`None`] when it could not be attested.
    pub uid: Option<u32>,
    /// The bundle it runs, [`None`] for a principal running none.
    pub app: Option<&'a AppIdentity>,
}

/// The authority deciding requests: where it serves, what it runs, and the
/// seams it decides through.
#[derive(Copy, Clone)]
pub struct Authority<'a> {
    /// Login's own console; a caller on any other is refused.
    pub console: u64,
    /// The bundle login runs, whose publisher the desktop session shares.
    pub app: Option<&'a AppIdentity>,
    /// Verifies an offered password.
    pub authenticator: &'a dyn Authenticator,
    /// Starts an allowed program.
    pub launcher: &'a dyn ElevateLauncher,
    /// Reads what a program's manifest requests.
    pub programs: &'a dyn ProgramRequests,
    /// The accounts, and the session presenting on the seat.
    pub directory: &'a dyn ElevateDirectory,
    /// Where every decision is audited.
    pub sink: &'a dyn Sink,
}

/// Read a captured run's output to end of stream, keeping the first
/// `out.len()` bytes.
///
/// `read` fills a buffer and answers how many bytes it took, `0` at end of
/// stream — the `Run` binary's pipe read; tests script it. The policy is
/// here rather than beside that syscall because its two edges are exactly
/// the ones a reader gets wrong: a stream that *exactly* fills `out` is
/// not an overrun (the buffer is only known to be too small once a further
/// read yields a byte), and the read continues past the bound regardless,
/// because a writer blocked on a full pipe never exits and a drain that
/// stopped early would hang the reap that follows it.
///
/// Answers how many bytes filled `out`, or [`None`] when the stream was
/// longer — in which case nothing is offered, because a prefix a caller
/// could mistake for the whole is worse than no answer.
///
/// # Errors
///
/// Whatever `read` raises, verbatim; nothing partial is reported.
pub fn drain_bounded(
    out: &mut [u8],
    mut read: impl FnMut(&mut [u8]) -> Result<usize, Errno>,
) -> Result<Option<usize>, Errno> {
    let mut filled = 0;
    while filled < out.len() {
        let Some(room) = out.get_mut(filled..) else {
            break;
        };
        let taken = read(room)?;
        if taken == 0 {
            return Ok(Some(filled));
        }
        filled = filled.saturating_add(taken);
    }
    let mut discard = [0u8; DRAIN_CHUNK];
    let mut overran = false;
    loop {
        if read(&mut discard)? == 0 {
            return Ok(if overran { None } else { Some(filled) });
        }
        overran = true;
    }
}

/// Bytes one discarding read takes once a captured run has already printed
/// past what the seam carries. Only the syscall count depends on it, so it
/// is sized to keep a runaway writer cheap to drain without a buffer worth
/// reserving.
const DRAIN_CHUNK: usize = 512;

/// Decide one elevation request, returning the reply to post.
///
/// The checks run strictly in trust order, each failing closed:
///
/// 1. **Placement** — the caller's attested console must be the authority's
///    own; a caller on another console, or on none, is refused before its
///    bytes are parsed.
/// 2. **Shape** — the request must decode, so a request that could never
///    run costs no attempt against the account it names.
/// 3. **Meter** — an account still cooling down is answered
///    [`ElevateReply::Cooling`] and nothing about it is checked.
/// 4. **Program** — what the program's manifest requests must be readable.
/// 5. **Re-authentication** — the named account's password, or for
///    [`ElevateRequest::Verify`] the caller's own attested account's, never
///    a name the request supplies.
/// 6. **Authority** — the account's ceiling must cover the program's
///    request.
/// 7. **Run** — a spawn refusal is reported verbatim.
///
/// A wrong password, an unknown account, a locked account, and an account
/// that may not allow the program are one answer,
/// [`Errno::PermissionDenied`], and each costs the account the same
/// attempt; the audit records which it was. [`ElevateRequest::Authorisers`]
/// has its own gate: only the presenting desktop may ask, and only about a
/// program in a system store.
///
/// `now` is the monotonic reading `meter` runs on. The caller owns the
/// request buffer and zeroises it (it carries the offered password) as soon
/// as this returns. `out` is the scratch a captured run's output, or an
/// account page, is written into, and the returned reply borrows it; it must
/// hold [`tairix_abi::elevate::ELEVATE_MAX_OUTPUT`] bytes, since a run that
/// prints more is reported as having overrun the seam rather than truncated
/// to fit it.
pub fn handle_elevate_request<'o>(
    bytes: &[u8],
    caller: &Caller<'_>,
    authority: &Authority<'_>,
    meter: &mut AttemptBudget,
    now: Duration64,
    out: &'o mut [u8],
) -> ElevateReply<'o> {
    if caller.console != authority.console {
        return refuse(authority.sink, Form::Run, None, refused("foreign console"));
    }
    let request = match ElevateRequest::decode(bytes) {
        Ok(request) => request,
        Err(err) => {
            let malformed = Refusal {
                cause: "malformed request",
                denial: Denial::Refused(err),
            };
            return refuse(authority.sink, Form::Run, None, malformed);
        }
    };
    let mut gate = Gate {
        authority,
        meter,
        now,
    };
    match request {
        ElevateRequest::Run {
            username,
            password,
            program,
            argv,
        } => handle_run(
            &mut gate,
            Credentials { username, password },
            Started { program, argv },
        ),
        ElevateRequest::Verify { password } => handle_verify(&mut gate, caller.uid, password),
        ElevateRequest::Launch {
            username,
            password,
            program,
        } => handle_launch(&mut gate, Credentials { username, password }, program),
        ElevateRequest::Capture {
            username,
            password,
            program,
            argv,
        } => handle_capture(
            &mut gate,
            Credentials { username, password },
            Started { program, argv },
            out,
        ),
        ElevateRequest::Authorisers { program, offset } => {
            handle_authorisers(authority, caller, program, offset, out)
        }
    }
}

/// The authority and its meter, as one request's checks use them.
struct Gate<'g, 'a> {
    authority: &'g Authority<'a>,
    meter: &'g mut AttemptBudget,
    now: Duration64,
}

impl Gate<'_, '_> {
    /// The checks every request naming an account shares — the meter, the
    /// program's request, the password, and the account's authority over
    /// the program — answering the account to run as.
    fn authorise(
        &mut self,
        credentials: &Credentials<'_>,
        program: &str,
    ) -> Result<AuthenticatedUser, Refusal> {
        let username = credentials.username;
        let retry_after = self.meter.retry_after(username, self.now);
        if retry_after > Duration64::ZERO {
            return Err(Refusal {
                cause: "cooling down",
                denial: Denial::Cooling(retry_after),
            });
        }
        let requested = self
            .authority
            .programs
            .requested(program)
            .map_err(|err| Refusal {
                cause: "manifest unreadable",
                denial: Denial::Refused(err),
            })?;
        let Ok(user) = self.authority.authenticator.authenticate(credentials) else {
            // The cause (wrong password, unknown, locked) is not recorded
            // beyond the offered name: those stay indistinguishable even to
            // an audit-log reader.
            self.meter.note_failure(username, self.now);
            return Err(refused("authentication failed"));
        };
        if !requested.is_subset_of(&user.capabilities) {
            // Charged as a wrong password is, so the meter cannot tell a
            // caller that this password was right.
            self.meter.note_failure(username, self.now);
            return Err(refused("account may not allow the program"));
        }
        self.meter.note_success(username);
        Ok(user)
    }
}

/// What a refused request is answered.
#[derive(Copy, Clone, Debug)]
enum Denial {
    /// Refused with this code — [`Errno::PermissionDenied`] for every
    /// authentication and authority failure alike.
    Refused(Errno),
    /// The account is cooling down, for this long; nothing was checked.
    Cooling(Duration64),
}

impl Denial {
    const fn reply(self) -> ElevateReply<'static> {
        match self {
            Self::Refused(err) => ElevateReply::Refused(err),
            Self::Cooling(retry_after) => ElevateReply::Cooling { retry_after },
        }
    }
}

/// A request refused before anything ran: why, for the audit, and what the
/// caller is told.
#[derive(Copy, Clone, Debug)]
struct Refusal {
    cause: &'static str,
    denial: Denial,
}

/// The refusal that discloses nothing: [`Errno::PermissionDenied`], with
/// `cause` kept for the audit alone.
const fn refused(cause: &'static str) -> Refusal {
    Refusal {
        cause,
        denial: Denial::Refused(Errno::PermissionDenied),
    }
}

/// Audit `refusal` under `form`'s event and answer it.
fn refuse(
    sink: &dyn Sink,
    form: Form,
    username: Option<&str>,
    refusal: Refusal,
) -> ElevateReply<'static> {
    audit_refusal(sink, form, username, refusal);
    refusal.denial.reply()
}

/// The named request forms, each audited under its own events.
#[derive(Copy, Clone, Debug)]
enum Form {
    Run,
    Launch,
    Capture,
}

impl Form {
    /// The event and message a refusal of this form is recorded under.
    const fn refused(self) -> (EventId, &'static str) {
        match self {
            Self::Run => (events::ELEVATE_REFUSED, "elevation refused"),
            Self::Launch => (events::LAUNCH_REFUSED, "elevated launch refused"),
            Self::Capture => (events::CAPTURE_REFUSED, "elevated capture refused"),
        }
    }
}

/// What a [`ElevateRequest::Run`] asks to be started: the program and the
/// arguments to hand it, carried together because neither is meaningful
/// without the other.
#[derive(Copy, Clone)]
struct Started<'a> {
    program: &'a str,
    argv: ElevateArgv<'a>,
}

/// Decide a [`ElevateRequest::Run`] request: authorise the named account
/// and, on success, run the program as it and wait for its exit.
fn handle_run(
    gate: &mut Gate<'_, '_>,
    credentials: Credentials<'_>,
    started: Started<'_>,
) -> ElevateReply<'static> {
    let sink = gate.authority.sink;
    let username = credentials.username;
    let user = match gate.authorise(&credentials, started.program) {
        Ok(user) => user,
        Err(refusal) => return refuse(sink, Form::Run, Some(username), refusal),
    };
    let launcher = gate.authority.launcher;
    match launcher.run_as(started.program, started.argv, user.uid.0) {
        Ok(exit_code) => {
            audit_granted(sink, username, started, user.uid.0, exit_code);
            ElevateReply::Completed { exit_code }
        }
        Err(err) => refuse(sink, Form::Run, Some(username), launch_failed(err)),
    }
}

/// Decide a [`ElevateRequest::Launch`] request: authorise the named account
/// and, on success, start `program` as it without waiting for it.
///
/// The identical checks as [`handle_run`]; only the reply differs, carrying
/// the started pid instead of an exit code the broker would have had to
/// block for.
fn handle_launch(
    gate: &mut Gate<'_, '_>,
    credentials: Credentials<'_>,
    program: &str,
) -> ElevateReply<'static> {
    let sink = gate.authority.sink;
    let username = credentials.username;
    let user = match gate.authorise(&credentials, program) {
        Ok(user) => user,
        Err(refusal) => return refuse(sink, Form::Launch, Some(username), refusal),
    };
    match gate.authority.launcher.launch_as(program, user.uid.0) {
        Ok(pid) => {
            audit_launch_granted(sink, username, program, user.uid.0, pid);
            ElevateReply::Launched { pid }
        }
        Err(err) => refuse(sink, Form::Launch, Some(username), launch_failed(err)),
    }
}

/// Decide a [`ElevateRequest::Capture`] request: authorise the named account
/// and, on success, run the program as it with its standard output drained
/// into `out`.
///
/// The identical checks as [`handle_run`]; what differs is where the child's
/// output goes and that the reply carries it. A run that printed more than
/// `out` holds is answered [`ElevateReply::Overran`]: the run happened and
/// its code is real, but no prefix is handed back to be read as the whole.
fn handle_capture<'o>(
    gate: &mut Gate<'_, '_>,
    credentials: Credentials<'_>,
    started: Started<'_>,
    out: &'o mut [u8],
) -> ElevateReply<'o> {
    let sink = gate.authority.sink;
    let username = credentials.username;
    let user = match gate.authorise(&credentials, started.program) {
        Ok(user) => user,
        Err(refusal) => return refuse(sink, Form::Capture, Some(username), refusal),
    };
    let launcher = gate.authority.launcher;
    let outcome = match launcher.capture_as(started.program, started.argv, user.uid.0, out) {
        Ok(outcome) => outcome,
        Err(err) => {
            let failed = Refusal {
                cause: "run failed",
                denial: Denial::Refused(err),
            };
            return refuse(sink, Form::Capture, Some(username), failed);
        }
    };
    match outcome {
        Captured::Output { exit_code, len } => {
            let Some(output) = out.get(..len) else {
                // A launcher reporting more bytes than the scratch holds
                // has answered nothing this end can stand behind.
                let overran = Refusal {
                    cause: "collector overran",
                    denial: Denial::Refused(Errno::OutOfRange),
                };
                return refuse(sink, Form::Capture, Some(username), overran);
            };
            audit_capture_granted(sink, username, started, user.uid.0, outcome);
            ElevateReply::Captured { exit_code, output }
        }
        Captured::Overran { exit_code } => {
            audit_capture_granted(sink, username, started, user.uid.0, outcome);
            ElevateReply::Overran { exit_code }
        }
    }
}

/// A spawn the launcher refused, reported to the caller verbatim.
const fn launch_failed(err: Errno) -> Refusal {
    Refusal {
        cause: "launch failed",
        denial: Denial::Refused(err),
    }
}

/// Decide a [`ElevateRequest::Verify`] request: re-authenticate the account
/// `uid` owns against `password`; run nothing.
///
/// An unattested caller is refused before any comparison, since there is no
/// account to check against. An attested uid that owns no account goes
/// through the identical `authenticate_uid` every other uid takes, so it
/// costs the same derivation and cannot be timed apart from a wrong password
/// on a real one; it has no account to meter.
fn handle_verify(
    gate: &mut Gate<'_, '_>,
    uid: Option<u32>,
    password: &str,
) -> ElevateReply<'static> {
    let sink = gate.authority.sink;
    let Some(uid) = uid else {
        return refuse_verify(sink, refused("no attested uid"));
    };
    let account = gate.authority.directory.login_name(uid);
    if let Some(account) = account {
        let retry_after = gate.meter.retry_after(account, gate.now);
        if retry_after > Duration64::ZERO {
            let cooling = Refusal {
                cause: "cooling down",
                denial: Denial::Cooling(retry_after),
            };
            return refuse_verify(sink, cooling);
        }
    }
    let verified = gate
        .authority
        .authenticator
        .authenticate_uid(uid, password)
        .is_ok();
    if let Some(account) = account {
        if verified {
            gate.meter.note_success(account);
        } else {
            gate.meter.note_failure(account, gate.now);
        }
    }
    if verified {
        audit_verify_granted(sink, uid);
        return ElevateReply::Verified;
    }
    refuse_verify(sink, refused("authentication failed"))
}

/// Audit a refused [`ElevateRequest::Verify`] and answer it.
fn refuse_verify(sink: &dyn Sink, refusal: Refusal) -> ElevateReply<'static> {
    audit_verify_refused(sink, refusal);
    refusal.denial.reply()
}

/// Decide an [`ElevateRequest::Authorisers`] request: tell the presenting
/// desktop which accounts may allow a system program.
///
/// The caller must be the session presenting on the seat — its attested uid
/// owns the foreground session — and must run the desktop bundle under the
/// authority's own publisher, so neither another of that user's programs nor
/// a bundle claiming the identifier can ask. The program must lie in a
/// read-only system store, so the answers are bounded to what the system
/// ships rather than to whatever request a caller writes into a manifest of
/// its own. Every answer and every refusal is audited.
fn handle_authorisers<'o>(
    authority: &Authority<'_>,
    caller: &Caller<'_>,
    program: &str,
    offset: u32,
    out: &'o mut [u8],
) -> ElevateReply<'o> {
    let sink = authority.sink;
    let presenting = caller
        .uid
        .filter(|uid| authority.directory.presenting() == Some(*uid));
    let desktop = caller
        .app
        .zip(authority.app)
        .is_some_and(|(app, own)| app.is_bundle(DESKTOP_BUNDLE, own.publisher()));
    let (Some(uid), true) = (presenting, desktop) else {
        return refuse_authorisers(sink, program, refused("not the presenting desktop"));
    };
    if !tairix_appstore::bundle_of_entry(program).is_some_and(tairix_appstore::is_machine_bundle) {
        return refuse_authorisers(sink, program, refused("not a system program"));
    }
    let requested = match authority.programs.requested(program) {
        Ok(requested) => requested,
        Err(err) => {
            let unreadable = Refusal {
                cause: "manifest unreadable",
                denial: Denial::Refused(err),
            };
            return refuse_authorisers(sink, program, unreadable);
        }
    };
    let page = authority.directory.authorisers(&requested, uid, offset);
    let len = encode_listing(out, page.total, offset, &page.accounts);
    audit_authorisers_sent(sink, program, uid, &page);
    ElevateReply::Authorisers {
        own: page.own,
        page: out.get(..len).unwrap_or_default(),
    }
}

/// Audit a refused [`ElevateRequest::Authorisers`] and answer it.
fn refuse_authorisers(sink: &dyn Sink, program: &str, refusal: Refusal) -> ElevateReply<'static> {
    audit_authorisers_refused(sink, program, refusal);
    refusal.denial.reply()
}

fn emit(sink: &dyn Sink, level: Level, id: EventId, message: &str, fields: &[Field<'_>]) {
    log(
        sink,
        &Event {
            level,
            id,
            message,
            fields,
        },
    );
}

/// Audit a granted run.
///
/// The argument vector is recorded as a **count**, never as content: the
/// broker hands the arguments over without interpreting them, so it cannot
/// know which of them is a secret — a tool that sets an account's password
/// would take one on its command line — and a log that might carry one is
/// worse than a log that carries none. What the elevated run changed is the
/// elevated tool's own to audit, where the authority and the meaning both
/// are.
fn audit_granted(sink: &dyn Sink, username: &str, started: Started<'_>, uid: u32, exit_code: i32) {
    let mut uid_buf = DecBuf::new();
    let mut code_buf = DecBuf::new();
    let mut args_buf = DecBuf::new();
    emit(
        sink,
        Level::Info,
        events::ELEVATE_GRANTED,
        "elevation granted",
        &[
            Field {
                key: "user",
                value: tairix_log::FieldValue::Str(username),
            },
            Field {
                key: "uid",
                value: tairix_log::FieldValue::Str(uid_buf.format(i128::from(uid))),
            },
            Field {
                key: "program",
                value: tairix_log::FieldValue::Str(started.program),
            },
            Field {
                key: "args",
                value: tairix_log::FieldValue::Str(
                    args_buf.format(i128::try_from(started.argv.len()).unwrap_or(i128::MAX)),
                ),
            },
            Field {
                key: "exit_code",
                value: tairix_log::FieldValue::Str(code_buf.format(i128::from(exit_code))),
            },
        ],
    );
}

/// Audit a refused named request under its form's event: why, for whom, and
/// what the caller was answered.
fn audit_refusal(sink: &dyn Sink, form: Form, username: Option<&str>, refusal: Refusal) {
    let (id, message) = form.refused();
    let mut answer_buf = DecBuf::new();
    emit(
        sink,
        Level::Warn,
        id,
        message,
        &[
            Field {
                key: "cause",
                value: tairix_log::FieldValue::Str(refusal.cause),
            },
            Field {
                key: "user",
                value: tairix_log::FieldValue::Str(username.unwrap_or("-")),
            },
            answer_field(&mut answer_buf, refusal.denial),
        ],
    );
}

/// What a refusal answered, as one audit field: the code, or how long the
/// account must wait.
fn answer_field(buf: &mut DecBuf, denial: Denial) -> Field<'_> {
    match denial {
        Denial::Refused(err) => Field {
            key: "errno",
            value: tairix_log::FieldValue::Str(buf.format(i128::from(err.as_i32()))),
        },
        Denial::Cooling(retry_after) => Field {
            key: "retry_after_s",
            value: tairix_log::FieldValue::Str(buf.format(i128::from(retry_after.secs()))),
        },
    }
}

fn audit_launch_granted(sink: &dyn Sink, username: &str, program: &str, uid: u32, pid: i64) {
    let mut uid_buf = DecBuf::new();
    let mut pid_buf = DecBuf::new();
    emit(
        sink,
        Level::Info,
        events::LAUNCH_GRANTED,
        "elevated program started",
        &[
            Field {
                key: "user",
                value: tairix_log::FieldValue::Str(username),
            },
            Field {
                key: "uid",
                value: tairix_log::FieldValue::Str(uid_buf.format(i128::from(uid))),
            },
            Field {
                key: "program",
                value: tairix_log::FieldValue::Str(program),
            },
            Field {
                key: "pid",
                value: tairix_log::FieldValue::Str(pid_buf.format(i128::from(pid))),
            },
        ],
    );
}

/// Audit a granted capture: what ran, and **how much** it printed back to
/// the caller — never a byte of what that was.
///
/// The bytes are the elevated program's output, which the broker relays
/// without reading, so it cannot know which of them is a secret; a log that
/// might carry one is worse than a log that carries none. Recording the
/// volume is what makes an unusual relay visible, exactly as the granted
/// run records the argument count.
fn audit_capture_granted(
    sink: &dyn Sink,
    username: &str,
    started: Started<'_>,
    uid: u32,
    outcome: Captured,
) {
    let (exit_code, relayed, disposition) = match outcome {
        Captured::Output { exit_code, len } => (exit_code, len, "returned"),
        // Nothing was relayed: the run printed past the seam's bound and
        // the caller was handed no bytes at all.
        Captured::Overran { exit_code } => (exit_code, 0, "overran"),
    };
    let mut uid_buf = DecBuf::new();
    let mut code_buf = DecBuf::new();
    let mut args_buf = DecBuf::new();
    let mut bytes_buf = DecBuf::new();
    emit(
        sink,
        Level::Info,
        events::CAPTURE_GRANTED,
        "elevated run returned its output",
        &[
            Field {
                key: "user",
                value: tairix_log::FieldValue::Str(username),
            },
            Field {
                key: "uid",
                value: tairix_log::FieldValue::Str(uid_buf.format(i128::from(uid))),
            },
            Field {
                key: "program",
                value: tairix_log::FieldValue::Str(started.program),
            },
            Field {
                key: "args",
                value: tairix_log::FieldValue::Str(
                    args_buf.format(i128::try_from(started.argv.len()).unwrap_or(i128::MAX)),
                ),
            },
            Field {
                key: "output",
                value: tairix_log::FieldValue::Str(disposition),
            },
            Field {
                key: "bytes",
                value: tairix_log::FieldValue::Str(
                    bytes_buf.format(i128::try_from(relayed).unwrap_or(i128::MAX)),
                ),
            },
            Field {
                key: "exit_code",
                value: tairix_log::FieldValue::Str(code_buf.format(i128::from(exit_code))),
            },
        ],
    );
}

/// Audit an answered [`ElevateRequest::Authorisers`]: the program, who asked,
/// whether its own account may allow it, and how many may — never which.
fn audit_authorisers_sent(sink: &dyn Sink, program: &str, uid: u32, page: &AuthoriserPage) {
    let mut uid_buf = DecBuf::new();
    let mut total_buf = DecBuf::new();
    emit(
        sink,
        Level::Info,
        events::AUTHORISERS_SENT,
        "accounts that may allow a program sent",
        &[
            Field {
                key: "program",
                value: tairix_log::FieldValue::Str(program),
            },
            Field {
                key: "uid",
                value: tairix_log::FieldValue::Str(uid_buf.format(i128::from(uid))),
            },
            Field {
                key: "own",
                value: tairix_log::FieldValue::Str(if page.own { "yes" } else { "no" }),
            },
            Field {
                key: "total",
                value: tairix_log::FieldValue::Str(total_buf.format(i128::from(page.total))),
            },
        ],
    );
}

fn audit_authorisers_refused(sink: &dyn Sink, program: &str, refusal: Refusal) {
    let mut answer_buf = DecBuf::new();
    emit(
        sink,
        Level::Warn,
        events::AUTHORISERS_REFUSED,
        "accounts that may allow a program withheld",
        &[
            Field {
                key: "cause",
                value: tairix_log::FieldValue::Str(refusal.cause),
            },
            Field {
                key: "program",
                value: tairix_log::FieldValue::Str(program),
            },
            answer_field(&mut answer_buf, refusal.denial),
        ],
    );
}

/// Audit a program started for an
/// [`ElevateRequest::Launch`] request that ended abnormally.
///
/// Login is the only component that can state this. Such a child inherits
/// login's console — under a graphical session the framebuffer text console
/// behind the desktop — so whatever it wrote to `stderr` before exiting
/// reaches nobody; the observer of the exit records it where a user can find
/// it. A clean exit (`status` 0) is a normal end and records nothing.
///
/// A status inside the reserved load-failure band is named in words through
/// the one shared reverse map ([`tairix_abi::load_failure_reason`]); any
/// other code is stated as the number alone, the most login can honestly say
/// about a program it did not write.
pub fn audit_launch_ended_abnormally(sink: &dyn Sink, pid: i64, status: i32) {
    if status == 0 {
        return;
    }
    let mut pid_buf = DecBuf::new();
    let mut status_buf = DecBuf::new();
    emit(
        sink,
        Level::Warn,
        events::LAUNCH_ENDED_ABNORMALLY,
        "elevated program ended abnormally",
        &[
            Field {
                key: "pid",
                value: tairix_log::FieldValue::Str(pid_buf.format(i128::from(pid))),
            },
            Field {
                key: "status",
                value: tairix_log::FieldValue::Str(status_buf.format(i128::from(status))),
            },
            Field {
                key: "reason",
                value: tairix_log::FieldValue::Str(
                    tairix_abi::load_failure_reason(status).unwrap_or("-"),
                ),
            },
        ],
    );
}

fn audit_verify_granted(sink: &dyn Sink, uid: u32) {
    let mut uid_buf = DecBuf::new();
    emit(
        sink,
        Level::Info,
        events::VERIFY_GRANTED,
        "verify-only elevation granted",
        &[Field {
            key: "uid",
            value: tairix_log::FieldValue::Str(uid_buf.format(i128::from(uid))),
        }],
    );
}

fn audit_verify_refused(sink: &dyn Sink, refusal: Refusal) {
    let mut answer_buf = DecBuf::new();
    emit(
        sink,
        Level::Warn,
        events::VERIFY_REFUSED,
        "verify-only elevation refused",
        &[
            Field {
                key: "cause",
                value: tairix_log::FieldValue::Str(refusal.cause),
            },
            answer_field(&mut answer_buf, refusal.denial),
        ],
    );
}

#[cfg(test)]
#[path = "elevate_tests.rs"]
mod tests;
