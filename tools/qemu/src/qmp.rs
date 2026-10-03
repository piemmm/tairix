//! The QEMU machine protocol: the control monitor a tap is sent over, since
//! the human monitor has no command that places a contact.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

/// How long QEMU has to answer one command, events between included, before
/// the run fails loud.
const ANSWER_WITHIN: Duration = Duration::from_secs(10);

/// One control-monitor connection, past its capabilities negotiation.
pub(crate) struct Qmp {
    stream: BufReader<UnixStream>,
}

impl Qmp {
    /// Connect to the control monitor at `path`, check its greeting, and
    /// leave negotiation mode.
    pub(crate) fn connect(path: &Path) -> io::Result<Self> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(ANSWER_WITHIN))?;
        let mut qmp = Self {
            stream: BufReader::new(stream),
        };
        let greeting = qmp.line(Instant::now() + ANSWER_WITHIN)?;
        if !greeting.starts_with(r#"{"QMP""#) {
            return Err(io::Error::other(format!(
                "not a control monitor: {}",
                greeting.trim_end()
            )));
        }
        qmp.execute(r#"{"execute":"qmp_capabilities"}"#)?;
        Ok(qmp)
    }

    /// Send `command` and wait for its answer, passing over the asynchronous
    /// events QEMU interleaves; a refusal is an error carrying QEMU's reason.
    pub(crate) fn execute(&mut self, command: &str) -> io::Result<()> {
        let stream = self.stream.get_mut();
        stream.write_all(command.as_bytes())?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        let deadline = Instant::now() + ANSWER_WITHIN;
        loop {
            let line = self.line(deadline)?;
            if line.starts_with(r#"{"return""#) {
                return Ok(());
            }
            if line.starts_with(r#"{"error""#) {
                return Err(io::Error::other(format!(
                    "QEMU refused {command}: {}",
                    line.trim_end()
                )));
            }
        }
    }

    /// The next line QEMU writes, if it writes one before `deadline`.
    fn line(&mut self, deadline: Instant) -> io::Result<String> {
        let silent = || io::Error::new(io::ErrorKind::TimedOut, "the control monitor fell silent");
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(silent());
        }
        if !self.stream.buffer().contains(&b'\n') {
            // macOS refuses options on a socket its peer has shut both ways.
            // Such a read cannot block, and still returns what the monitor
            // wrote before it went, so the timeout armed before stands.
            let _ = self.stream.get_ref().set_read_timeout(Some(remaining));
        }
        let mut line = String::new();
        match self.stream.read_line(&mut line) {
            Ok(0) => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the control monitor closed",
            )),
            Ok(_) => Ok(line),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                Err(silent())
            }
            Err(e) => Err(e),
        }
    }
}

/// The two `input-send-event` commands one tap is: contact `contact` landing
/// at (`x`, `y`) in the first slot, then lifting. QEMU closes each command's
/// events with one report, so the guest reads the touch and the lift as two
/// frames.
pub(crate) fn tap_commands(x: u32, y: u32, contact: u16) -> [String; 2] {
    let event = |kind: &str, tracking: i32, axis: &str, value: u32| {
        format!(
            r#"{{"type":"mtt","data":{{"type":"{kind}","slot":0,"tracking-id":{tracking},"axis":"{axis}","value":{value}}}}}"#
        )
    };
    let command = |events: &[String]| {
        format!(
            r#"{{"execute":"input-send-event","arguments":{{"events":[{}]}}}}"#,
            events.join(",")
        )
    };
    let id = i32::from(contact);
    // A tracking id of -1 is the slot protocol's lift.
    [
        command(&[
            event("begin", id, "x", x),
            event("data", id, "x", x),
            event("data", id, "y", y),
        ]),
        command(&[event("end", -1, "x", 0)]),
    ]
}

#[cfg(test)]
mod tests {
    use super::{tap_commands, Qmp};
    use std::io::{self, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    #[test]
    fn a_monitor_silent_until_the_deadline_fails_the_wait() {
        let (ours, _theirs) = UnixStream::pair().expect("a socket pair");
        let mut qmp = Qmp {
            stream: BufReader::new(ours),
        };
        let soon = Instant::now() + Duration::from_millis(20);
        let silent = qmp.line(soon).expect_err("nothing was said");
        assert_eq!(silent.kind(), io::ErrorKind::TimedOut);
        let spent = qmp.line(Instant::now()).expect_err("no time left");
        assert_eq!(spent.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn an_answer_written_before_the_monitor_hung_up_is_still_read() {
        let (ours, mut theirs) = UnixStream::pair().expect("a socket pair");
        theirs
            .write_all(b"{\"timestamp\": {}, \"event\": \"RESUME\"}\n{\"return\": {}}\n")
            .expect("the monitor answers");
        drop(theirs);
        let mut qmp = Qmp {
            stream: BufReader::new(ours),
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        assert!(qmp.line(deadline).expect("the event").contains("RESUME"));
        assert!(qmp
            .line(deadline)
            .expect("the answer")
            .starts_with(r#"{"return""#));
        let gone = qmp.line(deadline).expect_err("nothing follows the hang-up");
        assert_eq!(gone.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn a_tap_begins_and_places_its_contact_then_lifts_it() {
        let [touch, lift] = tap_commands(10, 20, 7);
        assert!(touch.starts_with(r#"{"execute":"input-send-event","arguments":{"events":["#));
        let begin = touch.find(r#""type":"begin","slot":0,"tracking-id":7"#);
        let across = touch.find(r#""type":"data","slot":0,"tracking-id":7,"axis":"x","value":10"#);
        let down = touch.find(r#""type":"data","slot":0,"tracking-id":7,"axis":"y","value":20"#);
        assert!(
            begin.is_some() && begin < across && across < down,
            "{touch}"
        );
        assert!(
            lift.contains(r#""type":"end","slot":0,"tracking-id":-1"#),
            "{lift}"
        );
        assert!(!lift.contains(r#""type":"data""#), "{lift}");
    }

    #[test]
    fn every_contact_id_stays_a_live_tracking_id() {
        let [touch, _] = tap_commands(0, 0, u16::MAX);
        assert!(touch.contains(r#""tracking-id":65535"#), "{touch}");
    }
}
