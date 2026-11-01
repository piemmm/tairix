use std::io::{PipeReader, PipeWriter, Read as _, Write as _};
use std::net::Shutdown;
use std::os::fd::AsRawFd as _;
use std::os::unix::net::UnixDatagram;
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tairix_qemu::Launch;

use super::{
    announce_or_die, mask, send_signal, send_signal_to_group, signal_child, spawn_clean, supervise,
    Children, Live, Starting, TERMINATING,
};
use crate::process::{answered, answering, ended_by, within, Lines, MINUTE, WHILE_ITS_PARENT_RUNS};
use crate::tests::{fill, first_line, running, this_test, with_a_grandchild, NO_ANSWER};

/// A grace no test outlasts: as a reap grace only a release ends its run,
/// and as a report grace only the report being taken ends its wait.
const OUTLASTING_GRACE: Duration = Duration::from_hours(1);

/// Set each `(signal, action)` in the child `cmd` forks before it execs,
/// whatever this test process has them at.
fn disposed(cmd: &mut Command, actions: Vec<(i32, libc::sighandler_t)>) {
    // SAFETY: runs in the forked child before it execs, and only calls
    // `signal`, which is async-signal-safe, over a list made before the fork.
    unsafe {
        cmd.pre_exec(move || {
            for &(signal, action) in &actions {
                libc::signal(signal, action);
            }
            Ok(())
        });
    }
}

/// `sh -c text`, with `signal` at its default action whatever this test
/// process inherited, so the child's fate does not depend on its runner.
fn script(text: &str, signal: i32) -> Command {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", text]).stdin(Stdio::piped());
    disposed(&mut cmd, vec![(signal, libc::SIG_DFL)]);
    cmd
}

/// `cmd` spawned as the leader of its own group, outside any registry, with
/// no signal blocked, as a fork is once it has announced itself.
fn spawned_alone(mut cmd: Command) -> Child {
    cmd.process_group(0);
    std::thread::spawn(move || {
        mask(libc::SIG_SETMASK, &[]).expect("cleared");
        cmd.spawn().expect("it spawns")
    })
    .join()
    .expect("spawned")
}

/// A child waited for on a thread of its own, released once the wait ends
/// as a budgeted run releases its own. Its stdin is held here, so a child
/// reading it to its end leaves with this test whatever stops the test.
struct Awaited {
    pid: u32,
    attached: bool,
    _stdin: Option<ChildStdin>,
    status: mpsc::Receiver<ExitStatus>,
}

impl Awaited {
    fn new(children: &Arc<Children>, mut child: Child, attached: bool) -> Self {
        let pid = child.id();
        let stdin = child.stdin.take();
        let children = Arc::clone(children);
        let status = answering(move || {
            let status = child.wait().expect("reaped");
            Launch::reaped(&*children, pid);
            status
        });
        Self {
            pid,
            attached,
            _stdin: stdin,
            status,
        }
    }

    /// The signal that ended the child, if it ended within a minute.
    fn ended_by(&self) -> Option<i32> {
        self.ended_within(MINUTE)
    }

    /// The signal that ended the child, if it ended within `wait`; if not, it
    /// is killed with everything it leads, so a failing test leaves nothing
    /// running.
    fn ended_within(&self, wait: Duration) -> Option<i32> {
        let status = self.status.recv_timeout(wait).ok();
        if status.is_none() {
            signal_child(self.pid, self.attached, libc::SIGKILL);
        }
        status.and_then(|status| status.signal())
    }
}

/// The process group `pid` is in.
fn group_of(pid: u32) -> libc::pid_t {
    let pid = libc::pid_t::try_from(pid).expect("a pid");
    // SAFETY: `getpgid` takes and returns scalars and touches no memory.
    unsafe { libc::getpgid(pid) }
}

#[test]
fn the_operators_floor_raises_a_budget_and_never_lowers_one() {
    let (short, long) = (Duration::from_secs(10), Duration::from_secs(100));
    let floored = Children::new(Some(Duration::from_mins(1)));
    assert_eq!(floored.raised(short), Duration::from_mins(1));
    assert_eq!(floored.raised(long), long);
    assert_eq!(Children::default().raised(short), short);
    assert_eq!(floored.ordinary_budget(), crate::DEFAULT_COMMAND_TIMEOUT);
    let slow = Duration::from_hours(2);
    assert_eq!(Children::new(Some(slow)).ordinary_budget(), slow);
}

#[test]
fn a_child_is_live_from_its_spawn_until_its_release() {
    let children = Arc::new(Children::default());
    let leader = children
        .spawn(&mut script("exec cat", libc::SIGTERM))
        .expect("cat spawns");
    let attached = Launch::spawn(&*children, &mut script("exec cat", libc::SIGTERM), true)
        .expect("cat spawns");
    let pids = [leader.id(), attached.id()];
    assert_eq!(children.live_children(), pids.into(), "both live");
    assert_eq!(
        group_of(leader.id()),
        libc::pid_t::try_from(leader.id()).expect("a pid"),
        "a leader leads a group of its own"
    );
    assert_eq!(
        group_of(attached.id()),
        group_of(0),
        "a child on the terminal stays in its spawner's group"
    );
    for (child, attached) in [(leader, false), (attached, true)] {
        let awaited = Awaited::new(&children, child, attached);
        send_signal(awaited.pid, libc::SIGTERM);
        assert_eq!(awaited.ended_by(), Some(libc::SIGTERM));
    }
    assert!(
        children.live_children().is_empty(),
        "released once waited for"
    );
}

#[test]
fn the_first_signal_reaches_every_child_and_a_second_kills_what_is_left() {
    let children = Arc::new(Children::default());
    // A leader with a grandchild in its group: the signal must reach both.
    let mut leader = script(&with_a_grandchild(), libc::SIGUSR1);
    leader.stdout(Stdio::piped());
    let mut leader = children.spawn(&mut leader).expect("sh spawns");
    let leader_pid = leader.id();
    let grandchild: u32 = first_line(&mut leader).parse().expect("a pid");
    let mut group_out = leader.stdout.take().expect("stdout is piped");
    let attached = Launch::spawn(&*children, &mut script("exec cat", libc::SIGUSR1), true)
        .expect("cat spawns");
    // Attached as well, so a child in xtask's own group alone holds the end
    // back once the others have gone.
    let mut stubborn = script("trap '' USR1; echo ready; exec cat", libc::SIGUSR1);
    stubborn.stdout(Stdio::piped());
    let mut stubborn = Launch::spawn(&*children, &mut stubborn, true).expect("sh spawns");
    // Read before the signal is sent, so the child ignores it however busy
    // the host is.
    assert_eq!(first_line(&mut stubborn), "ready");
    let leader = Awaited::new(&children, leader, false);
    let attached = Awaited::new(&children, attached, true);
    let stubborn = Awaited::new(&children, stubborn, true);

    children.take(libc::SIGUSR1);
    assert_eq!(leader.ended_by(), Some(libc::SIGUSR1), "the leader sent it");
    // The grandchild holds the group's stdout until it dies, and only the
    // group's signal ends it.
    let drained = within(move || group_out.read_to_end(&mut Vec::new()).is_ok());
    if drained != Some(true) {
        send_signal_to_group(leader_pid, libc::SIGKILL);
    }
    assert_eq!(
        drained,
        Some(true),
        "the leader's whole group gone, its grandchild {grandchild} with it"
    );
    assert_eq!(attached.ended_by(), Some(libc::SIGUSR1), "it alone sent it");

    let ended = Arc::clone(&children);
    let end = answering(move || ended.ended(OUTLASTING_GRACE));
    assert!(
        end.recv_timeout(NO_ANSWER).is_err(),
        "no end while a child is live"
    );
    children.take(libc::SIGTERM);
    assert_eq!(
        stubborn.ended_by(),
        Some(libc::SIGKILL),
        "killed at the second signal"
    );
    assert_eq!(
        end.recv_timeout(MINUTE).ok(),
        Some((libc::SIGUSR1, None)),
        "the run ends with its first signal once nothing is live"
    );
}

#[test]
fn a_child_that_outlasts_its_wait_is_killed_with_everything_it_leads() {
    let children = Arc::new(Children::default());
    let mut leader = script(&with_a_grandchild(), libc::SIGUSR1);
    leader.stdout(Stdio::piped());
    let mut leader = children.spawn(&mut leader).expect("sh spawns");
    let grandchild: u32 = first_line(&mut leader).parse().expect("a pid");
    let mut group_out = leader.stdout.take().expect("stdout is piped");
    let ended = Awaited::new(&children, leader, false).ended_within(NO_ANSWER);
    let drained = within(move || group_out.read_to_end(&mut Vec::new()).is_ok());
    assert_eq!(ended, None, "it outlasted its wait");
    assert_eq!(
        drained,
        Some(true),
        "its grandchild {grandchild} killed with it"
    );
}

#[test]
fn a_run_ends_within_a_reap_grace_of_its_kill_where_a_child_is_never_reaped() {
    let grace = Duration::from_millis(500);
    let children = Arc::new(Children::default());
    let mut unreaped = children
        .spawn(&mut script("exec cat", libc::SIGUSR1))
        .expect("cat spawns");
    let pid = unreaped.id();
    children.take(libc::SIGUSR1);
    let started = Instant::now();
    children.take(libc::SIGTERM);
    let ended = Arc::clone(&children);
    let end = within(move || ended.ended(grace));
    let took = started.elapsed();
    let _ = unreaped.wait();
    assert_eq!(
        end,
        Some((
            libc::SIGUSR1,
            Some(format!("pid {pid} killed but not yet reaped"))
        )),
        "ended with its first signal, naming what it left"
    );
    assert!(
        took >= grace,
        "only once the reap grace ran out, took {took:?}"
    );
}

#[test]
fn what_a_run_leaves_names_each_pid_and_counts_each_spawn_that_will_never_exec() {
    let spawn = |pid: Option<u32>| {
        let (mut starting, _theirs) = Starting::new(false).expect("a line");
        starting.pid = pid;
        starting
    };
    let mut live = Live::default();
    live.leaders.insert(10);
    assert_eq!(live.left_behind(), "pid 10 killed but not yet reaped");
    live.attached.insert(20);
    live.starting.insert(0, spawn(Some(30)));
    assert_eq!(
        live.left_behind(),
        "pids 10, 20, 30 killed but not yet reaped"
    );
    live.starting.insert(1, spawn(None));
    assert_eq!(
        live.left_behind(),
        "pids 10, 20, 30 killed but not yet reaped and 1 spawn that will never exec"
    );
    let mut spawning = Live::default();
    spawning.starting.insert(0, spawn(None));
    spawning.starting.insert(1, spawn(None));
    assert_eq!(spawning.left_behind(), "2 spawns that will never exec");
}

#[test]
fn settling_waits_out_an_ending_run_and_nothing_else() {
    let quiet = Arc::new(Children::default());
    assert_eq!(within(move || quiet.settle()), Some(()), "no run ending");
    let ending = Arc::new(Children::default());
    ending.take(libc::SIGUSR1);
    assert_eq!(
        answered(NO_ANSWER, move || ending.settle()),
        None,
        "an ending run's outcome waits for its signal"
    );
}

#[test]
fn nothing_starts_once_the_run_is_ending() {
    let children = Arc::new(Children::default());
    children.take(libc::SIGUSR1);
    let ending = Arc::clone(&children);
    let spawned = answered(NO_ANSWER, move || {
        ending.spawn(&mut script("exec cat", libc::SIGUSR1)).is_ok()
    });
    assert_eq!(spawned, None, "a spawn waits for the run's end");
    let live = children.live();
    assert_eq!(
        (live.serial, live.starting.len(), live.leaders.len()),
        (0, 0, 0),
        "nothing counted or forked"
    );
}

#[test]
fn a_child_starts_with_no_signal_blocked_whatever_its_spawner_blocks() {
    let children = Arc::new(Children::default());
    let spawner = Arc::clone(&children);
    let child = std::thread::spawn(move || {
        mask(libc::SIG_BLOCK, &[libc::SIGUSR1]).expect("blocked");
        spawner
            .spawn(&mut script("exec cat", libc::SIGUSR1))
            .expect("cat spawns")
    })
    .join()
    .expect("spawned");
    let awaited = Awaited::new(&children, child, false);
    children.take(libc::SIGUSR1);
    assert_eq!(
        awaited.ended_by(),
        Some(libc::SIGUSR1),
        "the spawner's mask is not the child's"
    );
}

#[test]
fn a_fork_announces_its_pid_and_never_execs_once_told_a_signal() {
    let (ours, theirs) = UnixDatagram::pair().expect("a line");
    ours.set_nonblocking(true).expect("never waited on");
    assert!(
        announce_or_die(theirs.as_raw_fd()).is_ok(),
        "told nothing, it goes on to exec"
    );
    let mut pid = [0_u8; 4];
    assert_eq!(ours.recv(&mut pid).ok(), Some(4), "announced");
    assert_eq!(
        u32::try_from(i32::from_ne_bytes(pid)).ok(),
        Some(std::process::id())
    );
    // The null signal comes back from the raise, so the fork answers that it
    // will not exec without this process being sent anything.
    ours.send(&0_i32.to_ne_bytes()).expect("told");
    assert_eq!(
        announce_or_die(theirs.as_raw_fd()).map_err(|err| err.raw_os_error()),
        Err(Some(libc::ECANCELED))
    );
    // Shut rather than dropped: another test's fork may hold a copy of it.
    ours.shutdown(Shutdown::Read).expect("shut");
    assert!(
        announce_or_die(theirs.as_raw_fd()).is_err(),
        "unable to announce itself, it never execs either"
    );
}

#[test]
fn the_run_reaches_a_fork_that_announced_itself_before_it_was_told() {
    let children = Arc::new(Children::default());
    // Stands in for a fork that has announced itself and is stalled in its
    // exec.
    let stalled = spawned_alone(script("exec cat", libc::SIGUSR1));
    let (serial, theirs) = children.begin(false).expect("counted");
    theirs.set_nonblocking(true).expect("never waited on");
    let pid = i32::try_from(stalled.id()).expect("a pid");
    theirs.send(&pid.to_ne_bytes()).expect("announced");
    let stalled = Awaited::new(&children, stalled, false);
    children.take(libc::SIGUSR1);
    let ended = stalled.ended_by();
    let mut told = [0_u8; 4];
    let heard = theirs.recv(&mut told).ok();
    // Its spawn resolves to a child of its own the signal never reached, so
    // only what this test sends it ends it.
    let mut cmd = script("exec cat", libc::SIGUSR1);
    disposed(&mut cmd, vec![(libc::SIGTERM, libc::SIG_DFL)]);
    let resolved = spawned_alone(cmd);
    let resolved_pid = resolved.id();
    let resolved = Awaited::new(&children, resolved, false);
    children.finish(serial, Some(resolved_pid));
    send_signal(resolved_pid, libc::SIGTERM);
    assert_eq!(
        ended,
        Some(libc::SIGUSR1),
        "sent the signal at the pid it announced"
    );
    assert_eq!(heard, Some(4), "and told it");
    assert_eq!(i32::from_ne_bytes(told), libc::SIGUSR1);
    assert_eq!(
        resolved.ended_by(),
        Some(libc::SIGTERM),
        "and not sent it again as its spawn resolved"
    );
}

#[test]
fn a_spawn_whose_word_was_lost_is_sent_what_it_owes_as_it_resolves() {
    for (signals, deaf, owed) in [
        (&[libc::SIGUSR1][..], false, libc::SIGUSR1),
        (&[libc::SIGUSR1, libc::SIGTERM][..], true, libc::SIGKILL),
    ] {
        let children = Arc::new(Children::default());
        let (serial, theirs) = children.begin(false).expect("counted");
        // The fork's end shut, so the run's word to it is lost.
        theirs.shutdown(Shutdown::Read).expect("shut");
        for &signal in signals {
            children.take(signal);
        }
        // Stands in for a fork that heard nothing and exec'd.
        let mut cmd = script("exec cat", libc::SIGUSR1);
        if deaf {
            disposed(&mut cmd, vec![(libc::SIGUSR1, libc::SIG_IGN)]);
        }
        let execd = spawned_alone(cmd);
        let pid = execd.id();
        let execd = Awaited::new(&children, execd, false);
        children.finish(serial, Some(pid));
        assert_eq!(execd.ended_by(), Some(owed), "after {signals:?}");
    }
}

#[test]
fn a_fork_made_once_the_run_is_ending_dies_of_what_it_owes_before_it_execs() {
    for (signals, deaf, owed) in [
        (&[libc::SIGUSR1][..], false, libc::SIGUSR1),
        (&[libc::SIGUSR1, libc::SIGTERM][..], true, libc::SIGKILL),
    ] {
        let children = Arc::new(Children::default());
        // Counted before the signals, as a step under way is, and forked
        // after them; never finished, so nothing but its own word can end it.
        let (_serial, line) = children.begin(false).expect("counted");
        for &signal in signals {
            children.take(signal);
        }
        let mut cmd = script("exec cat", libc::SIGUSR1);
        if deaf {
            disposed(&mut cmd, vec![(libc::SIGUSR1, libc::SIG_IGN)]);
        }
        let child = spawn_clean(&mut cmd, false, &line).expect("it spawns");
        let died = Awaited::new(&children, child, false).ended_by();
        assert_eq!(died, Some(owed), "after {signals:?}");
    }
}

#[test]
fn a_command_spawned_again_announces_on_its_new_line_alone() {
    // Both lines stay open throughout, so a stale closure still holding the
    // first line's descriptor would announce the second child on it.
    let lines = [(); 2].map(|()| {
        let (ours, theirs) = UnixDatagram::pair().expect("a line");
        ours.set_nonblocking(true).expect("never waited on");
        (ours, theirs)
    });
    let mut cmd = Command::new("true");
    let pids = lines.each_ref().map(|(_, theirs)| {
        let mut child = spawn_clean(&mut cmd, false, theirs).expect("true spawns");
        child.wait().expect("reaped");
        child.id()
    });
    for ((ours, _), pid) in lines.iter().zip(pids) {
        let mut heard = Vec::new();
        let mut word = [0_u8; 4];
        while let Ok(4) = ours.recv(&mut word) {
            heard.push(u32::try_from(i32::from_ne_bytes(word)).expect("a pid"));
        }
        assert_eq!(heard, [pid], "announced once, on its own spawn's line");
    }
}

/// How long a litmus thread spins before it parks, given `cpus` to run on and
/// what one `round` costs: a few rounds where the two threads can race, and
/// none where they share one CPU and cannot.
fn spin_budget(cpus: usize, round: Duration) -> Duration {
    if cpus > 1 {
        round * 4
    } else {
        Duration::ZERO
    }
}

/// What one round of the exchange costs on this host: the median of a few.
fn round_cost() -> Duration {
    let mut costs: Vec<Duration> = (0..32)
        .map(|_| {
            let started = Instant::now();
            let (mut starting, theirs) = Starting::new(true).expect("a line");
            starting.owe(0);
            let _ = announce_or_die(theirs.as_raw_fd());
            started.elapsed()
        })
        .collect();
    costs.sort_unstable();
    costs[costs.len() / 2]
}

/// Wait for `flag` to reach `round`, spinning for `budget` to keep the race
/// tight while both threads run, then parking.
fn wait_for(flag: &AtomicU32, round: u32, budget: Duration) {
    let until = Instant::now() + budget;
    loop {
        for _ in 0..32 {
            if flag.load(Ordering::Acquire) == round {
                return;
            }
            std::hint::spin_loop();
        }
        if Instant::now() >= until {
            break;
        }
    }
    while flag.load(Ordering::Acquire) != round {
        std::thread::park();
    }
}

#[test]
fn a_litmus_thread_spins_only_where_its_partner_can_run() {
    let round = Duration::from_micros(5);
    assert_eq!(spin_budget(1, round), Duration::ZERO, "alone on its CPU");
    assert_eq!(spin_budget(2, round), round * 4);
}

/// The exchange's order, raced on two threads round after round: in none may
/// the fork hear no word while the run learns no pid.
#[test]
fn a_fork_and_the_run_never_both_miss_each_other() {
    const ROUNDS: u32 = 100_000;
    let armed = Arc::new(AtomicU32::new(0));
    let done = Arc::new(AtomicU32::new(0));
    let line = Arc::new(AtomicI32::new(-1));
    let told = Arc::new(AtomicBool::new(false));
    let cpus = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    let budget = spin_budget(cpus, round_cost());
    let run = std::thread::current();
    let fork = {
        let (armed, done, line, told) = (
            Arc::clone(&armed),
            Arc::clone(&done),
            Arc::clone(&line),
            Arc::clone(&told),
        );
        std::thread::spawn(move || {
            for round in 1..=ROUNDS {
                wait_for(&armed, round, budget);
                let heard = announce_or_die(line.load(Ordering::Relaxed)).is_err();
                told.store(heard, Ordering::Relaxed);
                done.store(round, Ordering::Release);
                run.unpark();
            }
        })
    };
    let mut missed = 0;
    for round in 1..=ROUNDS {
        // Attached, so the pid it learns, this process's own, is sent the
        // null signal alone.
        let (mut starting, theirs) = Starting::new(true).expect("a line");
        line.store(theirs.as_raw_fd(), Ordering::Relaxed);
        armed.store(round, Ordering::Release);
        starting.owe(0);
        fork.thread().unpark();
        let learned = starting.pid.is_some();
        wait_for(&done, round, budget);
        if !learned && !told.load(Ordering::Relaxed) {
            missed += 1;
        }
    }
    fork.join().expect("the fork's thread");
    assert_eq!(missed, 0, "rounds where each missed the other");
}

/// A spawn of a command held in its child before it execs, until let go.
///
/// A held fork keeps every descriptor its process had, another hold's
/// release end among them, so only one hold may be under way in a process at
/// a time. Then closing the fork's own copy of its release end leaves the
/// test's the only one, and the test's death lets the exec go rather than
/// hold it for ever.
struct Held {
    spawned: JoinHandle<std::io::Result<Child>>,
    release: PipeWriter,
    ready: PipeReader,
    _reader: PipeReader,
}

impl Held {
    fn new(children: &Arc<Children>, mut cmd: Command) -> Self {
        let (held, release) = std::io::pipe().expect("a pipe");
        let (ready, ready_end) = std::io::pipe().expect("a pipe");
        let (fd, own_end, says_ready) =
            (held.as_raw_fd(), release.as_raw_fd(), ready_end.as_raw_fd());
        // SAFETY: runs in the forked child before it execs, and only calls
        // `close`, `write` and `read`, all async-signal-safe.
        unsafe {
            cmd.pre_exec(move || {
                libc::close(own_end);
                let mut byte = 0_u8;
                libc::write(says_ready, (&raw const byte).cast(), 1);
                libc::read(fd, (&raw mut byte).cast(), 1);
                Ok(())
            });
        }
        let owner = Arc::clone(children);
        // Dropped once the spawn returns, so one that never forks ends the
        // wait for its hold.
        let spawned = std::thread::spawn(move || {
            let spawned = owner.spawn(&mut cmd);
            drop(ready_end);
            spawned
        });
        Self {
            spawned,
            release,
            ready,
            _reader: held,
        }
    }

    /// Once the fork says it is held, `act` on `children` on a thread of its
    /// own, answering whether it did in time: an act held up behind a stalled
    /// exec answers no, and the caller lets the exec go whatever happened.
    fn while_held(
        &self,
        children: &Arc<Children>,
        act: impl FnOnce(&Children) + Send + 'static,
    ) -> bool {
        let mut ready = self.ready.try_clone().expect("the hold's word");
        let children = Arc::clone(children);
        within(move || {
            let held = ready.read_exact(&mut [0_u8]).is_ok();
            if held {
                act(&children);
            }
            held
        })
        .unwrap_or(false)
    }

    /// Let the held exec go, answering the child it starts.
    fn let_go(mut self) -> Child {
        self.release.write_all(b"x").expect("its exec let go");
        self.spawned
            .join()
            .expect("the spawn returns")
            .expect("it spawns")
    }
}

/// Both scenarios run here one after the other, as [`Held`] allows only one
/// hold at a time.
#[test]
fn a_child_forked_as_the_run_ends_dies_of_the_signal_it_owes() {
    held_as_the_run_begins_to_end();
    held_as_a_second_signal_comes();
}

fn held_as_the_run_begins_to_end() {
    let children = Arc::new(Children::default());
    let held = Held::new(&children, script("exec cat", libc::SIGUSR1));
    let sent = held.while_held(&children, |children| children.take(libc::SIGUSR1));
    let ended = Arc::clone(&children);
    let end = answering(move || ended.ended(OUTLASTING_GRACE));
    let early = end.recv_timeout(NO_ANSWER).ok();
    let late = Awaited::new(&children, held.let_go(), false).ended_by();
    assert!(sent, "the signal waited on a stalled exec");
    assert_eq!(early, None, "no end while a spawn is under way");
    assert_eq!(late, Some(libc::SIGUSR1), "dead of the signal it owed");
    assert_eq!(
        end.recv_timeout(MINUTE).ok(),
        Some((libc::SIGUSR1, None)),
        "the run ends once it is gone"
    );
}

fn held_as_a_second_signal_comes() {
    let children = Arc::new(Children::default());
    let mut deaf = script("exec cat", libc::SIGUSR1);
    disposed(&mut deaf, vec![(libc::SIGUSR1, libc::SIG_IGN)]);
    let held = Held::new(&children, deaf);
    let sent = held.while_held(&children, |children| {
        children.take(libc::SIGUSR1);
        children.take(libc::SIGTERM);
    });
    let late = Awaited::new(&children, held.let_go(), false).ended_by();
    assert!(sent, "the signals waited on a stalled exec");
    assert_eq!(late, Some(libc::SIGKILL), "dead of the kill it owed last");
}

/// Set to run [`signal_helper`], its child the script this names, as the
/// process the forwarding tests start.
const HELPER: &str = "TAIRIX_XTASK_SIGNAL_HELPER";

/// How [`signal_helper`] runs: [`RELEASED`] unless this names another way.
const HELPER_MODE: &str = "TAIRIX_XTASK_SIGNAL_HELPER_MODE";

/// Its child released once reaped, as a budgeted run releases its own.
const RELEASED: &str = "released";

/// Its child never released, so only the reap grace, [`UNRELEASED_GRACE`],
/// ends the run once a second signal has come, the report it then makes
/// waited for only the run's own report grace.
const UNRELEASED: &str = "unreleased";

/// [`UNRELEASED`], its report waited for however long it takes.
const REPORTED: &str = "reported";

/// The reap grace after which an unreleased helper ends its run, once it has
/// made its report or given up waiting for it.
const UNRELEASED_GRACE: Duration = Duration::from_millis(200);

/// Beside its child, one that ignores the first signal and is released only
/// once it ends, so the run's outcome comes before its end.
const STUBBORN: &str = "stubborn";

/// What [`signal_helper`] names its child's pid after, wherever in a line
/// its test harness leaves it.
const CHILD: &str = "signal-helper-child-pid";

/// What a [`STUBBORN`] helper names its stubborn child's pid after.
const STUBBORN_CHILD: &str = "signal-helper-stubborn-pid";

/// What [`signal_helper`] writes how its child ended after.
const CHILD_ENDED: &str = "signal-helper-child-ended";

/// What [`signal_helper`] writes once its run's outcome is in, if the run
/// leaves it to.
const ENDED_ON_ITS_OWN: &str = "signal-helper-ended-on-its-own";

/// A child script that says once it is ready to take a quit, marks each
/// quit it takes and carries on, ending only with the process that started
/// it.
fn takes_a_quit() -> String {
    format!("trap 'echo quit-taken' QUIT; echo quit-trapped; {WHILE_ITS_PARENT_RUNS}")
}

/// Not a test of its own: the process the forwarding tests start, set up as
/// xtask's `main` is, doing nothing unless one of them started it.
#[test]
fn signal_helper() {
    let Ok(child_script) = std::env::var(HELPER) else {
        return;
    };
    let mode = std::env::var(HELPER_MODE).unwrap_or_else(|_| RELEASED.to_owned());
    // Its harness's threads keep the block they started with, so none of
    // them can take a signal; this one starts as xtask's main does.
    mask(libc::SIG_UNBLOCK, &TERMINATING).expect("unblocked");
    // A run that ends of a quit leaves no core of this process: no core file
    // where cores go to files, and on Linux, where they may go to a program
    // that ignores that limit, none at all.
    let no_core = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `setrlimit` only reads `no_core`, which outlives the call.
    unsafe {
        libc::setrlimit(libc::RLIMIT_CORE, &raw const no_core);
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: marking this process undumpable touches no memory of ours.
        unsafe {
            libc::prctl(libc::PR_SET_DUMPABLE, 0);
        }
    }
    let children = Arc::new(Children::default());
    let unreleased = mode == UNRELEASED || mode == REPORTED;
    let (grace, report_grace) = match mode.as_str() {
        UNRELEASED => (UNRELEASED_GRACE, super::REPORT_GRACE),
        REPORTED => (UNRELEASED_GRACE, OUTLASTING_GRACE),
        _ => (OUTLASTING_GRACE, super::REPORT_GRACE),
    };
    let outcome = supervise(&children, grace, report_grace, || {
        let stubborn = (mode == STUBBORN).then(|| {
            let mut cmd = script("trap '' INT; echo ready; exec cat", libc::SIGTERM);
            cmd.stdout(Stdio::piped());
            let mut child = children.spawn(&mut cmd).expect("sh spawns");
            // Read before any pid is named, so the first signal finds its
            // trap set however busy the host is.
            assert_eq!(first_line(&mut child), "ready");
            outln!("{STUBBORN_CHILD} {}", child.id()).expect("reported");
            Awaited::new(&children, child, false)
        });
        let mut cmd = Command::new("sh");
        cmd.args(["-c", &child_script]).stdin(Stdio::piped());
        let mut child = children.spawn(&mut cmd).expect("sh spawns");
        let _stdin = child.stdin.take();
        outln!("{CHILD} {}", child.id()).expect("reported");
        let status = child.wait().expect("reaped");
        // Before the release, which lets the run end.
        outln!("{CHILD_ENDED} {:?}", status.signal()).expect("reported");
        if !unreleased {
            children.release(child.id());
        }
        stubborn
    })
    .expect("forwarding");
    drop(outcome);
    outln!("{ENDED_ON_ITS_OWN}").expect("reported");
}

/// Start [`signal_helper`] in `mode`, its child running `child_script` and
/// its stderr `stderr`, with the terminating signals blocked, so no thread
/// of its test harness can take one, and each at its default but `ignored`;
/// answer it, its child's pid and its lines.
fn helper(
    ignored: Option<i32>,
    child_script: &str,
    mode: &str,
    stderr: Stdio,
) -> (Child, u32, Lines) {
    let mut cmd = this_test("children::tests::signal_helper");
    cmd.arg("--nocapture")
        .env(HELPER, child_script)
        .env(HELPER_MODE, mode)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(stderr);
    // Whatever this test's runner ignored, as a shell's background job
    // ignores SIGINT, the helper starts as a terminal would leave it, but for
    // what the test asks it to ignore.
    let mut actions: Vec<_> = TERMINATING
        .iter()
        .map(|&signal| (signal, libc::SIG_DFL))
        .collect();
    actions.extend(ignored.map(|signal| (signal, libc::SIG_IGN)));
    disposed(&mut cmd, actions);
    // The child inherits the spawning thread's mask.
    let mut helper = std::thread::spawn(move || {
        mask(libc::SIG_BLOCK, &TERMINATING).expect("blocked");
        cmd.spawn().expect("the helper spawns")
    })
    .join()
    .expect("spawned");
    let mut lines = Lines::new(helper.stdout.take().expect("stdout is piped"));
    let Some(child) = lines.after(CHILD).and_then(|pid| pid.parse().ok()) else {
        send_signal(helper.id(), libc::SIGKILL);
        let _ = helper.wait();
        panic!("the helper never named its child");
    };
    (helper, child, lines)
}

/// How `helper` said its child ended, and the signal that ended `helper`
/// itself, if each came within a minute.
fn helper_ended(helper: Child, lines: &mut Lines) -> (Option<String>, Option<i32>) {
    let child_ended = lines.after(CHILD_ENDED);
    (child_ended, ended_by(helper))
}

#[test]
fn a_terminating_signal_reaches_every_child_and_ends_xtask_with_it() {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let (helper, child, mut lines) = helper(None, "exec cat", RELEASED, Stdio::null());
        send_signal(helper.id(), signal);
        let (child_ended, ended) = helper_ended(helper, &mut lines);
        assert_eq!(
            child_ended,
            Some(format!("{:?}", Some(signal))),
            "its child ended by signal {signal}, passed on"
        );
        assert_eq!(ended, Some(signal), "and then it, by signal {signal}");
        assert!(!running(child), "its child {child} gone");
    }
}

#[test]
fn a_quit_is_passed_on_too_and_a_second_signal_kills_what_survives_it() {
    let (helper, child, mut lines) = helper(None, &takes_a_quit(), RELEASED, Stdio::null());
    // A quit that came before the trap would end the child of its default
    // action, core and all, so none is sent until the trap is set.
    if lines.after("quit-trapped") != Some(String::new()) {
        send_signal(helper.id(), libc::SIGKILL);
        let _ = ended_by(helper);
        panic!("its child never set its trap");
    }
    send_signal(helper.id(), libc::SIGQUIT);
    // Taken before the second is sent: a lower-numbered signal pending beside
    // it would be taken first.
    let quit_taken = lines.after("quit-taken");
    send_signal(helper.id(), libc::SIGINT);
    let (child_ended, ended) = helper_ended(helper, &mut lines);
    assert_eq!(
        quit_taken,
        Some(String::new()),
        "its child took the quit passed on"
    );
    assert_eq!(
        child_ended,
        Some(format!("{:?}", Some(libc::SIGKILL))),
        "killed at the second signal"
    );
    assert_eq!(ended, Some(libc::SIGQUIT), "and the run ended by the first");
    assert!(!running(child), "its child {child} gone");
}

#[test]
fn a_signal_xtask_started_ignoring_stays_ignored() {
    let (helper, child, mut lines) =
        helper(Some(libc::SIGHUP), "exec cat", RELEASED, Stdio::null());
    send_signal(helper.id(), libc::SIGHUP);
    send_signal(helper.id(), libc::SIGINT);
    let (child_ended, ended) = helper_ended(helper, &mut lines);
    assert_eq!(
        child_ended,
        Some(format!("{:?}", Some(libc::SIGINT))),
        "the hang-up left ignored, its child ended by the interrupt"
    );
    assert_eq!(ended, Some(libc::SIGINT));
    assert!(!running(child), "its child {child} gone");
}

#[test]
fn an_ending_run_s_outcome_waits_for_its_end() {
    let (helper, child, mut lines) = helper(None, "exec cat", STUBBORN, Stdio::null());
    let stubborn: Option<u32> = lines.after(STUBBORN_CHILD).and_then(|pid| pid.parse().ok());
    send_signal(helper.id(), libc::SIGINT);
    let child_ended = lines.after(CHILD_ENDED);
    // Its outcome is in, but a child that ignores the first signal is still
    // live, so only a second can end the run.
    let early = lines.after_within(ENDED_ON_ITS_OWN, NO_ANSWER);
    send_signal(helper.id(), libc::SIGTERM);
    let ended = ended_by(helper);
    let late = lines.after(ENDED_ON_ITS_OWN);
    assert_eq!(
        child_ended,
        Some(format!("{:?}", Some(libc::SIGINT))),
        "its child ended by the interrupt"
    );
    assert_eq!(
        (early, late),
        (None, None),
        "the run's end, not its outcome, ended it"
    );
    assert_eq!(ended, Some(libc::SIGINT), "by its first signal");
    assert!(
        stubborn.is_some_and(|pid| !running(pid)),
        "its stubborn child {stubborn:?} gone"
    );
    assert!(!running(child), "its child {child} gone");
}

/// Interrupt `helper` and, once it says its child has ended, terminate it, as
/// ends an unreleased run at its reap grace; answer how its child ended.
fn interrupted_then_terminated(helper: &Child, lines: &mut Lines) -> Option<String> {
    send_signal(helper.id(), libc::SIGINT);
    let child_ended = lines.after(CHILD_ENDED);
    send_signal(helper.id(), libc::SIGTERM);
    child_ended
}

#[test]
fn a_report_no_one_reads_does_not_hold_the_end() {
    let (unread, stderr) = std::io::pipe().expect("a pipe");
    fill(&stderr);
    let (helper, _child, mut lines) = helper(None, "exec cat", UNRELEASED, Stdio::from(stderr));
    let child_ended = interrupted_then_terminated(&helper, &mut lines);
    let ended = ended_by(helper);
    drop(unread);
    assert_eq!(
        child_ended,
        Some(format!("{:?}", Some(libc::SIGINT))),
        "its child ended by the interrupt"
    );
    assert_eq!(
        ended,
        Some(libc::SIGINT),
        "and it, though its report was never read"
    );
}

#[test]
fn a_run_ended_at_its_reap_grace_says_what_it_left() {
    let (mut said, stderr) = std::io::pipe().expect("a pipe");
    // Full, so its report waits on this test's reading rather than a clock.
    fill(&stderr);
    let (helper, child, mut lines) = helper(None, "exec cat", REPORTED, Stdio::from(stderr));
    let child_ended = interrupted_then_terminated(&helper, &mut lines);
    let ending = answering(move || ended_by(helper));
    let early = ending.recv_timeout(2 * super::REPORT_GRACE).ok();
    let report = within(move || {
        let mut text = String::new();
        let _ = said.read_to_string(&mut text);
        text
    });
    let ended = ending.recv_timeout(MINUTE).ok().flatten();
    assert_eq!(
        child_ended,
        Some(format!("{:?}", Some(libc::SIGINT))),
        "its child ended by the interrupt"
    );
    assert_eq!(early, None, "its report waited to be read");
    let line = format!(
        "xtask: ending {UNRELEASED_GRACE:?} after the kill with pid {child} killed but not yet \
         reaped\n"
    );
    assert!(
        report.is_some_and(|report| report.ends_with(&line)),
        "{line:?}"
    );
    assert_eq!(ended, Some(libc::SIGINT), "by its first signal");
}

/// Set to run [`unstartable_helper`] as the process its test starts.
const UNSTARTABLE: &str = "TAIRIX_XTASK_UNSTARTABLE_HELPER";

/// What [`unstartable_helper`] writes whether its run was refused after.
const REFUSED: &str = "unstartable-helper-refused";

/// Not a test of its own: a run begun where no thread can start, then
/// interrupted, doing nothing unless that test started it.
#[test]
fn unstartable_helper() {
    if std::env::var_os(UNSTARTABLE).is_none() {
        return;
    }
    mask(libc::SIG_UNBLOCK, &TERMINATING).expect("unblocked");
    let children = Arc::new(Children::default());
    let refused = supervise(
        &children,
        super::KILL_REAP_GRACE,
        super::REPORT_GRACE,
        || (),
    )
    .is_err();
    outln!("{REFUSED} {refused}").expect("reported");
    // SAFETY: `raise` takes and returns scalars and touches no memory of ours.
    unsafe {
        libc::raise(libc::SIGINT);
    }
}

#[test]
fn a_run_whose_threads_cannot_start_is_still_ended_by_an_interrupt() {
    let mut cmd = this_test("children::tests::unstartable_helper");
    cmd.arg("--nocapture")
        .env(UNSTARTABLE, "1")
        // A default stack no address space holds, so no thread starts.
        .env("RUST_MIN_STACK", (1_u64 << 50).to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    disposed(&mut cmd, vec![(libc::SIGINT, libc::SIG_DFL)]);
    let mut helper = cmd.spawn().expect("the helper spawns");
    let mut lines = Lines::new(helper.stdout.take().expect("stdout is piped"));
    let refused = lines.after(REFUSED);
    let ended = ended_by(helper);
    assert_eq!(
        refused.as_deref(),
        Some("true"),
        "its threads could not start"
    );
    assert_eq!(ended, Some(libc::SIGINT), "and the interrupt ended it");
}

/// Set to run [`launch_note_helper`] as the process the note test starts.
const LAUNCH_NOTE: &str = "TAIRIX_XTASK_LAUNCH_NOTE_HELPER";

/// Not a test of its own: the process the note test starts, doing nothing
/// unless that test started it.
#[test]
fn launch_note_helper() {
    if std::env::var_os(LAUNCH_NOTE).is_some() {
        Launch::note(&Children::default(), format_args!("tairix-qemu: a note"));
        noteln!("xtask: the next line");
    }
}

#[test]
fn what_tools_qemu_notes_is_a_line_of_its_own() {
    let output = this_test("children::tests::launch_note_helper")
        .arg("--nocapture")
        .env(LAUNCH_NOTE, "1")
        .stdin(Stdio::null())
        .output()
        .expect("the helper runs");
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("tairix-qemu: a note\nxtask: the next line\n"),
        "{output:?}"
    );
}
