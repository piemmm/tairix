//! The children xtask spawns: the floor their budgets are raised to, the
//! process groups they lead, and the terminal's signals passed on to them.
//!
//! Every child but a session on the terminal leads a process group of its
//! own, so a budget can kill everything it forked. That takes it out of the
//! terminal's foreground group, though, which a Ctrl-C or a hang-up reaches,
//! so xtask takes the terminating signals itself: every thread blocks them
//! for one that waits on them, and every child starts with that mask cleared.
//! The first such signal goes on to every live child, a leader's whole group
//! with it, and xtask ends with that signal once they have all gone; a second
//! kills every child still live, and xtask ends within a reap grace of it,
//! and a moment to say what it left, even where one cannot be reaped. A child
//! whose exec has yet to resolve is reached as well: its fork announces its
//! pid before it execs, then dies of the signal it owes instead if the run
//! has begun to end, and one whose pid went unread is sent it as its spawn
//! resolves. A signal xtask was started with ignored, as `nohup` leaves a
//! hang-up, stays ignored.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::os::fd::{AsRawFd as _, RawFd};
use std::os::unix::net::UnixDatagram;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::DEFAULT_COMMAND_TIMEOUT;

/// How long a process group is given to exit on `SIGTERM` before a budget's
/// kill sends `SIGKILL`: short, as the step has already failed, but long
/// enough for a well-behaved child to exit cleanly.
const GRACE: Duration = Duration::from_secs(2);

/// How long a child sent `SIGKILL` is given to be reaped. Bounded, as a
/// process wedged in an uninterruptible kernel sleep would hold its wait, and
/// whatever waits on that, for as long as the sleep lasted.
pub(crate) const KILL_REAP_GRACE: Duration = Duration::from_secs(5);

/// How long the end waits for stderr to take its last report: a stream no one
/// reads must not hold the end.
pub(crate) const REPORT_GRACE: Duration = Duration::from_secs(1);

/// The children xtask spawns.
#[derive(Default)]
pub(crate) struct Children {
    /// The operator's `$TAIRIX_XTASK_TIMEOUT_SECS`, read once as the run
    /// starts: it only ever raises a budget.
    floor: Option<Duration>,
    live: Mutex<Live>,
    changed: Condvar,
}

#[derive(Default)]
struct Live {
    /// Children leading process groups of their own.
    leaders: BTreeSet<u32>,
    /// Children left in xtask's own group, as a session on the terminal is.
    attached: BTreeSet<u32>,
    /// Spawns whose exec has yet to resolve, by the serial each is counted
    /// under.
    starting: BTreeMap<u64, Starting>,
    /// The serial the next spawn is counted under.
    serial: u64,
    /// How the run is ending, once a terminating signal is taken.
    ending: Option<Ending>,
}

/// A spawn whose exec has yet to resolve.
struct Starting {
    attached: bool,
    /// Where its fork announces its pid, and is told what the run's end owes.
    line: UnixDatagram,
    /// The pid its fork announced, once read.
    pid: Option<u32>,
}

/// How a run that has taken a terminating signal is ending.
#[derive(Clone, Copy)]
struct Ending {
    /// The first signal taken, which xtask ends with.
    signal: i32,
    /// When a second signal was taken, from which xtask waits only a reap
    /// grace for its children to be reaped.
    killed: Option<Instant>,
}

impl Ending {
    /// What a child is sent: the first signal, or `SIGKILL` once a second
    /// has been taken.
    fn owed(self) -> i32 {
        if self.killed.is_some() {
            KILL
        } else {
            self.signal
        }
    }
}

impl Live {
    fn is_empty(&self) -> bool {
        self.leaders.is_empty() && self.attached.is_empty() && self.starting.is_empty()
    }

    /// Send `signal` to every live child, a leader's whole group with it, and
    /// owe it to every spawn under way.
    fn signal_all(&mut self, signal: i32) {
        for &leader in &self.leaders {
            signal_child(leader, false, signal);
        }
        for &pid in &self.attached {
            signal_child(pid, true, signal);
        }
        for starting in self.starting.values_mut() {
            starting.owe(signal);
        }
    }

    /// What is still live once the reap grace has run out.
    fn left_behind(&self) -> String {
        let mut pids: BTreeSet<u32> = self.leaders.union(&self.attached).copied().collect();
        pids.extend(self.starting.values().filter_map(|starting| starting.pid));
        let unannounced = self
            .starting
            .values()
            .filter(|starting| starting.pid.is_none())
            .count();
        let mut left = Vec::new();
        if !pids.is_empty() {
            let named: Vec<String> = pids.iter().map(u32::to_string).collect();
            let noun = if named.len() == 1 { "pid" } else { "pids" };
            left.push(format!(
                "{noun} {} killed but not yet reaped",
                named.join(", ")
            ));
        }
        match unannounced {
            0 => {}
            1 => left.push("1 spawn that will never exec".to_owned()),
            n => left.push(format!("{n} spawns that will never exec")),
        }
        left.join(" and ")
    }
}

impl Starting {
    /// A spawn under way, and the end of its line its fork is to hold.
    fn new(attached: bool) -> io::Result<(Self, UnixDatagram)> {
        let (line, theirs) = UnixDatagram::pair()?;
        // Read under the lock, so never waited on.
        line.set_nonblocking(true)?;
        Ok((
            Self {
                attached,
                line,
                pid: None,
            },
            theirs,
        ))
    }

    /// Tell the fork it owes `signal`, then send it the signal where it has
    /// announced its pid. Told first and asked after: a fork that looked for
    /// word before this told it had announced itself already, so the ask
    /// finds it.
    fn owe(&mut self, signal: i32) {
        let _ = self.line.send(&signal.to_ne_bytes());
        if self.pid.is_none() {
            let mut pid = [0_u8; 4];
            if let Ok(4) = self.line.recv(&mut pid) {
                self.pid = u32::try_from(i32::from_ne_bytes(pid)).ok();
            }
        }
        if let Some(pid) = self.pid {
            signal_child(pid, self.attached, signal);
        }
    }
}

impl Children {
    /// Children whose budgets are raised to `floor`.
    pub(crate) fn new(floor: Option<Duration>) -> Self {
        Self {
            floor,
            ..Self::default()
        }
    }

    /// `budget` raised to the operator's floor, where one is set.
    pub(crate) fn raised(&self, budget: Duration) -> Duration {
        self.floor.map_or(budget, |floor| budget.max(floor))
    }

    /// The budget of a command spawned directly rather than run as a step:
    /// [`DEFAULT_COMMAND_TIMEOUT`], raised.
    pub(crate) fn ordinary_budget(&self) -> Duration {
        self.raised(DEFAULT_COMMAND_TIMEOUT)
    }

    fn live(&self) -> MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait<'a>(&self, live: MutexGuard<'a, Live>) -> MutexGuard<'a, Live> {
        self.changed
            .wait(live)
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Spawn `cmd` as the leader of its own process group, live until it is
    /// [released](Self::release).
    pub(crate) fn spawn(&self, cmd: &mut Command) -> io::Result<Child> {
        self.launch(cmd, false)
    }

    /// Spawn `cmd`, in xtask's own group where `attached` holds, live until
    /// it is released. Once the run is ending this waits for its end instead:
    /// nothing new starts as everything else stops.
    fn launch(&self, cmd: &mut Command, attached: bool) -> io::Result<Child> {
        let (serial, line) = self.begin(attached)?;
        // Not under the lock: an exec stalled in the kernel must not hold up
        // the signal every live child is owed.
        let spawned = spawn_clean(cmd, attached, &line);
        self.finish(serial, spawned.as_ref().ok().map(Child::id));
        spawned
    }

    /// Count a spawn as under way once the run is not ending, answering the
    /// serial it is counted under and the end of its line its fork holds.
    fn begin(&self, attached: bool) -> io::Result<(u64, UnixDatagram)> {
        let (starting, theirs) = Starting::new(attached)?;
        let mut live = self.live();
        while live.ending.is_some() {
            live = self.wait(live);
        }
        let serial = live.serial;
        live.serial += 1;
        live.starting.insert(serial, starting);
        Ok((serial, theirs))
    }

    /// The spawn counted under `serial` has resolved, to the child `pid`
    /// where it started.
    fn finish(&self, serial: u64, pid: Option<u32>) {
        let mut live = self.live();
        let spawn = live.starting.remove(&serial);
        if let (Some(spawn), Some(pid)) = (spawn, pid) {
            // A pid left unread may mean a lost datagram let the fork exec
            // unsignalled; a fork dead of its own raise is unreaped, so the
            // signal reaches nothing else.
            if let Some(ending) = live.ending.filter(|_| spawn.pid.is_none()) {
                signal_child(pid, spawn.attached, ending.owed());
            }
            if spawn.attached {
                live.attached.insert(pid);
            } else {
                live.leaders.insert(pid);
            }
        }
        drop(live);
        self.changed.notify_all();
    }

    /// `pid`, a child this spawned, has been reaped, or killed and given up
    /// on.
    pub(crate) fn release(&self, pid: u32) {
        let mut live = self.live();
        live.leaders.remove(&pid);
        live.attached.remove(&pid);
        drop(live);
        self.changed.notify_all();
    }

    /// Return unless the run is ending, and otherwise wait for its end.
    fn settle(&self) {
        let mut live = self.live();
        while live.ending.is_some() {
            live = self.wait(live);
        }
    }

    /// Take a terminating signal: the first passes on to every live child as
    /// the run's end begins, and any later one kills what is still live.
    fn take(&self, signal: i32) {
        let mut live = self.live();
        let ending = match live.ending {
            None => Ending {
                signal,
                killed: None,
            },
            Some(ending) => Ending {
                killed: ending.killed.or_else(|| Some(Instant::now())),
                ..ending
            },
        };
        live.ending = Some(ending);
        live.signal_all(ending.owed());
        drop(live);
        self.changed.notify_all();
    }

    /// The signal the run ends on, once it is ending and no child is live or
    /// starting, or once `grace` has passed since a second signal killed
    /// them, with what was then still left.
    fn ended(&self, grace: Duration) -> (i32, Option<String>) {
        let mut live = self.live();
        loop {
            let Some(ending) = live.ending else {
                live = self.wait(live);
                continue;
            };
            if live.is_empty() {
                return (ending.signal, None);
            }
            let Some(deadline) = ending.killed.and_then(|killed| killed.checked_add(grace)) else {
                live = self.wait(live);
                continue;
            };
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return (ending.signal, Some(live.left_behind()));
            }
            live = self
                .changed
                .wait_timeout(live, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// The leaders and attached children still live, for another module's
    /// tests.
    #[cfg(test)]
    pub(crate) fn live_children(&self) -> BTreeSet<u32> {
        let live = self.live();
        live.leaders.union(&live.attached).copied().collect()
    }
}

impl tairix_qemu::Launch for Children {
    fn spawn(&self, cmd: &mut Command, on_terminal: bool) -> io::Result<Child> {
        self.launch(cmd, on_terminal)
    }

    fn reaped(&self, pid: u32) {
        self.release(pid);
    }

    fn note(&self, args: std::fmt::Arguments<'_>) {
        noteln!("{args}");
    }
}

/// Spawn `cmd` with no signal blocked, as the leader of its own process group
/// (so the group id is its pid) unless `attached`, its fork announcing itself
/// on `line` before it execs.
///
/// A child inherits the mask of the thread that spawns it, and in xtask every
/// thread blocks the terminating signals for the one that waits on them, so
/// left as it is, a child could never be stopped by any of them. Clearing it
/// costs `std` its `posix_spawn` path, a fork per child, which is nothing
/// beside what any child does.
fn spawn_clean(cmd: &mut Command, attached: bool, line: &UnixDatagram) -> io::Result<Child> {
    use std::os::unix::process::CommandExt as _;

    // A command spawned again keeps this closure, so it finds its line
    // through a cell emptied once this spawn has resolved, and then does
    // nothing.
    let line = Arc::new(AtomicI32::new(line.as_raw_fd()));
    let in_fork = Arc::clone(&line);
    // SAFETY: the closure runs in the forked child before it execs, and calls
    // only async-signal-safe functions: `setpgid`, and `mask` and
    // `announce_or_die`, which allocate nothing and touch nothing but the
    // child's own state.
    unsafe {
        cmd.pre_exec(move || match in_fork.load(Ordering::Relaxed) {
            -1 => Ok(()),
            line => {
                // Leading its group before it announces the pid the group is
                // signalled by.
                if !attached && libc::setpgid(0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                mask(libc::SIG_SETMASK, &[])?;
                announce_or_die(line)
            }
        });
    }
    let spawned = cmd.spawn();
    line.store(-1, Ordering::Relaxed);
    spawned
}

/// In a fork about to exec: announce its pid on `line`, then die of the
/// latest signal the run has told it of, if any, rather than exec. A fork
/// that cannot announce itself never execs either.
fn announce_or_die(line: RawFd) -> io::Result<()> {
    let mut owed = None;
    // SAFETY: `getpid`, `send`, `recv` and `raise` are async-signal-safe, as a
    // fork of a threaded process needs, and each buffer outlives the call it
    // is passed to with its own length.
    unsafe {
        let pid = libc::getpid().to_ne_bytes();
        if libc::send(line, pid.as_ptr().cast(), pid.len(), libc::MSG_DONTWAIT) != 4 {
            return Err(io::Error::last_os_error());
        }
        let mut signal = [0_u8; 4];
        while libc::recv(
            line,
            signal.as_mut_ptr().cast(),
            signal.len(),
            libc::MSG_DONTWAIT,
        ) == 4
        {
            owed = Some(i32::from_ne_bytes(signal));
        }
        if let Some(signal) = owed {
            libc::raise(signal);
        }
    }
    match owed {
        // Only a signal the child ignores comes back, and it still never execs.
        Some(_) => Err(io::Error::from_raw_os_error(libc::ECANCELED)),
        None => Ok(()),
    }
}

/// `SIGKILL`, which no child can block, ignore or catch.
const KILL: i32 = libc::SIGKILL;

/// Kills the process group `leader` leads: `SIGTERM`, the [`GRACE`], then
/// `SIGKILL`.
pub(crate) fn kill(leader: u32) {
    send_signal_to_group(leader, libc::SIGTERM);
    std::thread::sleep(GRACE);
    send_signal_to_group(leader, libc::SIGKILL);
}

/// Send `signal` to `pid`, or to the whole group it leads unless `attached`.
fn signal_child(pid: u32, attached: bool, signal: i32) {
    if attached {
        send_signal(pid, signal);
    } else {
        send_signal_to_group(pid, signal);
    }
}

/// Sends `signal` to the process group `leader` leads, best-effort: a group
/// already gone is the success case.
fn send_signal_to_group(leader: u32, signal: i32) {
    let Ok(group) = libc::pid_t::try_from(leader) else {
        return;
    };
    // SAFETY: `killpg` is a plain signal-delivery syscall with no memory
    // operands, so the only soundness obligation is passing well-formed
    // scalars: `group` is a pid one of this process's own children was
    // spawned under as its group's leader, converted without truncation, and
    // `signal` a valid signal number.
    unsafe {
        libc::killpg(group, signal);
    }
}

/// Sends `signal` to the process `pid`, best-effort as
/// [`send_signal_to_group`] is.
fn send_signal(pid: u32, signal: i32) {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    // SAFETY: `kill` is a plain signal-delivery syscall with no memory
    // operands; `pid` is one a child was spawned as or `getpid` answered in
    // `announce_or_die`, never 0, and converted without truncation, so it is
    // positive and names that process alone.
    unsafe {
        libc::kill(pid, signal);
    }
}

/// The signals that end xtask, each passed on to every live child first.
const TERMINATING: [i32; 4] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT];

/// Run `command` with the terminating signals taken as this module describes,
/// a second ending xtask `grace` after its kill, and `report_grace` after that
/// at most, even where a child cannot be reaped; answer its outcome once the
/// run is not ending, as an ending run's outcome must not end xtask before its
/// signal does.
///
/// Called before any other thread starts, so every thread inherits the mask
/// that leaves the signals to the one that takes them.
pub(crate) fn supervise<T>(
    children: &Arc<Children>,
    grace: Duration,
    report_grace: Duration,
    command: impl FnOnce() -> T,
) -> Result<T, String> {
    forward_signals(Arc::clone(children), grace, report_grace)?;
    let outcome = command();
    children.settle();
    Ok(outcome)
}

/// Take the terminating signals xtask was not started ignoring on a thread
/// of their own from now on, passing each on to `children`, and end xtask
/// with the first once no child is left, or `grace` after a second signal
/// even if one still is, once it has said what it left or `report_grace` has
/// passed. Where a thread cannot start, the signals are left unblocked, as
/// nothing would take them.
fn forward_signals(
    children: Arc<Children>,
    grace: Duration,
    report_grace: Duration,
) -> Result<(), String> {
    let taken: Vec<i32> = TERMINATING
        .into_iter()
        .filter(|&signal| !ignored(signal))
        .collect();
    if taken.is_empty() {
        return Ok(());
    }
    mask(libc::SIG_BLOCK, &taken)
        .map_err(|err| format!("cannot block the terminating signals: {err}"))?;
    take_on_threads(children, set_of(&taken), grace, report_grace).map_err(|err| {
        match mask(libc::SIG_UNBLOCK, &taken) {
            Ok(()) => err,
            Err(unblock) => format!("{err}, and cannot unblock the terminating signals: {unblock}"),
        }
    })
}

/// Start the thread that ends a run and the one that takes `set`'s signals,
/// as [`forward_signals`] describes.
fn take_on_threads(
    children: Arc<Children>,
    set: libc::sigset_t,
    grace: Duration,
    report_grace: Duration,
) -> Result<(), String> {
    let ender = Arc::clone(&children);
    std::thread::Builder::new()
        .name("xtask-end".into())
        .spawn(move || {
            let (signal, left) = ender.ended(grace);
            if let Some(left) = left {
                report(
                    format!("xtask: ending {grace:?} after the kill with {left}\n"),
                    report_grace,
                );
            }
            end_with(signal)
        })
        .map_err(|err| format!("cannot start the thread that ends a run: {err}"))?;
    std::thread::Builder::new()
        .name("xtask-signals".into())
        .spawn(move || loop {
            match next_signal(&set) {
                Ok(signal) => children.take(signal),
                // Unreachable for a valid set; the run ends rather than leave
                // every later signal pending and unanswered.
                Err(err) => {
                    noteln!("xtask: cannot wait for a terminating signal ({err}); ending the run");
                    children.take(libc::SIGTERM);
                    break;
                }
            }
        })
        .map(drop)
        .map_err(|err| format!("cannot start the signal thread: {err}"))
}

/// Write `text` to stderr on a thread of its own, waiting at most `grace` for
/// it.
fn report(text: String, grace: Duration) {
    let (written, done) = mpsc::channel();
    let reporting = std::thread::Builder::new()
        .name("xtask-report".into())
        .spawn(move || {
            crate::note(&text);
            let _ = written.send(());
        });
    if reporting.is_ok() {
        let _ = done.recv_timeout(grace);
    }
}

/// Whether `signal` was ignored when xtask started, which leaves it ignored.
fn ignored(signal: i32) -> bool {
    let mut action = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
    // SAFETY: with no new action, `sigaction` only writes the current one
    // into `action`; a C library need not write all of it (the kernel hands
    // back only its own short mask), and the zero fill it starts from is a
    // valid `sigaction` whatever it leaves.
    unsafe {
        libc::sigaction(signal, std::ptr::null(), action.as_mut_ptr()) == 0
            && action.assume_init().sa_sigaction == libc::SIG_IGN
    }
}

fn set_of(signals: &[i32]) -> libc::sigset_t {
    let mut set = std::mem::MaybeUninit::<libc::sigset_t>::zeroed();
    // SAFETY: the set starts zeroed, a valid `sigset_t`, which `sigemptyset`
    // need clear only as far as its signals reach and `sigaddset` only sets
    // bits of valid signal numbers within.
    unsafe {
        libc::sigemptyset(set.as_mut_ptr());
        for &signal in signals {
            libc::sigaddset(set.as_mut_ptr(), signal);
        }
        set.assume_init()
    }
}

/// Change this thread's signal mask by `signals`, as `how` says.
fn mask(how: i32, signals: &[i32]) -> io::Result<()> {
    let set = set_of(signals);
    // SAFETY: `set` is an initialised signal set the call only reads, and
    // changing the mask touches no memory of ours.
    match unsafe { libc::pthread_sigmask(how, &raw const set, std::ptr::null_mut()) } {
        0 => Ok(()),
        err => Err(io::Error::from_raw_os_error(err)),
    }
}

/// The next of `set`'s signals to arrive, all of them blocked.
fn next_signal(set: &libc::sigset_t) -> io::Result<i32> {
    let mut signal = 0;
    // SAFETY: `set` is an initialised signal set the call only reads, and
    // `signal` outlives the call, which only writes it.
    let waited = unsafe { libc::sigwait(set, &raw mut signal) };
    if waited == 0 {
        Ok(signal)
    } else {
        Err(io::Error::from_raw_os_error(waited))
    }
}

/// End xtask as `signal` would have: its default action, taken on this thread.
fn end_with(signal: i32) -> ! {
    // SAFETY: restoring a signal's default disposition touches no memory of
    // ours.
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
    }
    let _ = mask(libc::SIG_UNBLOCK, &[signal]);
    // SAFETY: raising a signal touches no memory of ours; at its default
    // action it ends the process.
    unsafe {
        libc::raise(signal);
    }
    // Reached only for a signal whose default action does not end a process.
    std::process::exit(128 + signal)
}

#[cfg(test)]
#[path = "children_tests.rs"]
mod tests;
