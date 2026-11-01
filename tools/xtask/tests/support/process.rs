//! Watching a process a test started: the lines it writes, how it ended, and
//! a bounded wait for what it does; and the loop that keeps one alive only as
//! long as its parent.

use std::io::{BufRead as _, BufReader};
use std::os::unix::process::ExitStatusExt as _;
use std::process::{Child, ChildStdout};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long a test waits for what it watches.
pub(crate) const MINUTE: Duration = Duration::from_mins(1);

/// A shell command that runs for as long as its shell's parent does, and
/// takes a trapped signal as it comes rather than after the sleep it is in.
pub(crate) const WHILE_ITS_PARENT_RUNS: &str =
    r#"while kill -0 "$PPID" 2>/dev/null; do sleep 1 & wait $!; done"#;

/// What `f` answers, once it does, computed on a thread of its own.
pub(crate) fn answering<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> mpsc::Receiver<T> {
    let (sent, answer) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sent.send(f());
    });
    answer
}

/// What `f` answers on a thread of its own within `wait`, if it does.
pub(crate) fn answered<T: Send + 'static>(
    wait: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    answering(f).recv_timeout(wait).ok()
}

/// What `f` answers within a minute, if it does.
pub(crate) fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    answered(MINUTE, f)
}

/// Send `signal` to `pid`.
pub(crate) fn signal(pid: u32, signal: i32) {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    // SAFETY: `kill` takes and returns scalars and touches no memory of ours.
    unsafe {
        libc::kill(pid, signal);
    }
}

/// A process's lines as they arrive, and those no marker has claimed yet,
/// which a later one may: what it starts can write before it does.
pub(crate) struct Lines {
    arriving: mpsc::Receiver<String>,
    unclaimed: Vec<String>,
}

impl Lines {
    /// The lines `stdout` carries, read on a thread of their own.
    pub(crate) fn new(stdout: ChildStdout) -> Self {
        let (sent, arriving) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sent.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            arriving,
            unclaimed: Vec::new(),
        }
    }

    /// What follows `marker` in the first line holding it, waiting until a
    /// minute has passed or the lines end.
    pub(crate) fn after(&mut self, marker: &str) -> Option<String> {
        self.after_within(marker, MINUTE)
    }

    /// [`Lines::after`], waiting only `wait`.
    pub(crate) fn after_within(&mut self, marker: &str, wait: Duration) -> Option<String> {
        let rest = |line: &str| {
            line.rsplit_once(marker)
                .map(|(_, rest)| rest.trim().to_owned())
        };
        if let Some(at) = self.unclaimed.iter().position(|line| line.contains(marker)) {
            return rest(&self.unclaimed.remove(at));
        }
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self.arriving.recv_timeout(left).ok()?;
            if line.contains(marker) {
                return rest(&line);
            }
            self.unclaimed.push(line);
        }
    }
}

/// The signal that ended `child`, if it ended within a minute; one still
/// running is killed.
pub(crate) fn ended_by(mut child: Child) -> Option<i32> {
    let pid = child.id();
    let ended = within(move || child.wait().ok()).flatten();
    if ended.is_none() {
        signal(pid, libc::SIGKILL);
    }
    ended.and_then(|status| status.signal())
}
