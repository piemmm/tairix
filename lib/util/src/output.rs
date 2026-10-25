//! The byte stream a command engine writes through.
//!
//! A command app's engine writes its output and diagnostics through an
//! injected [`Output`] rather than a descriptor, so a host test captures
//! exactly what it renders and a refused write is an ordinary result.
//! `lib/rt` implements it for the standard streams, so a `Run` binary hands
//! its engine `&Stdout` and `&Stderr` themselves.

use tairix_abi::Errno;

/// A byte stream an engine writes through.
pub trait Output {
    /// Write every byte of `bytes`.
    ///
    /// # Errors
    ///
    /// The stream's own refusal: a closed terminal, a broken pipe, a revoked
    /// descriptor. A stream that stops taking bytes partway is an error,
    /// never a short success.
    fn write_all(&self, bytes: &[u8]) -> Result<(), Errno>;

    /// Emit one framed advisory record on the standard information stream
    /// (fd 3), best-effort.
    ///
    /// The stream is ignorable, so a missing consumer or a refused record
    /// never changes the command's output or status. The default drops the
    /// record: a sink with no advisory consumer.
    fn info(&self, record: &[u8]) {
        let _ = record;
    }

    /// Whether the stream is an attested terminal, which decides how a name
    /// from elsewhere is written to it ([`crate::quote::output_style`]). The
    /// default answers no: a pipe, a file, a sink a test captures.
    fn at_terminal(&self) -> bool {
        false
    }
}

/// Report `failure` on `err` as every command app names one — its own word,
/// a colon, the failure — in one write: `rm: cannot remove 'x': not found`.
///
/// # Errors
///
/// The stream's own refusal.
pub fn report_failure(
    err: &dyn Output,
    tool: &str,
    failure: impl core::fmt::Display,
) -> Result<(), Errno> {
    err.write_all(alloc::format!("{tool}: {failure}\n").as_bytes())
}

/// A failure a command either carries on past or ends on.
pub trait Failure: core::fmt::Display {
    /// Whether the failure ends the run, rather than only the operand or
    /// entry it befell.
    fn is_fatal(&self) -> bool;

    /// The failure the diagnostic stream refusing a report is.
    fn unreported(errno: Errno) -> Self;
}

/// The failures a run carries on past, as GNU's tools carry on: each is
/// reported under the command's word as it happens, and the run is
/// remembered as failed, so it can end with the failing status.
pub struct Carried<'a> {
    err: &'a dyn Output,
    tool: &'a str,
    silenced: bool,
    clean: bool,
}

impl<'a> Carried<'a> {
    /// A run reporting on `err` under `tool`'s word.
    #[must_use]
    pub const fn new(err: &'a dyn Output, tool: &'a str) -> Self {
        Self {
            err,
            tool,
            silenced: false,
            clean: true,
        }
    }

    /// The same run, reporting nothing it carries on past when `silenced`
    /// (`chmod -f`), yet still ending failed.
    #[must_use]
    pub const fn silenced(self, silenced: bool) -> Self {
        Self { silenced, ..self }
    }

    /// `done`'s value, or [`None`] once its failure is reported and carried
    /// past.
    ///
    /// # Errors
    ///
    /// A fatal failure, or [`Failure::unreported`] when the stream refuses
    /// the report.
    pub fn absorb<T, E: Failure>(&mut self, done: Result<T, E>) -> Result<Option<T>, E> {
        match done {
            Ok(value) => Ok(Some(value)),
            Err(failure) => self.carry(failure).map(|()| None),
        }
    }

    /// Report `failure` and carry on past it, or hand it back when it ends
    /// the run.
    ///
    /// # Errors
    ///
    /// A fatal `failure`, or [`Failure::unreported`] when the stream refuses
    /// the report.
    pub fn carry<E: Failure>(&mut self, failure: E) -> Result<(), E> {
        if failure.is_fatal() {
            return Err(failure);
        }
        self.clean = false;
        if self.silenced {
            return Ok(());
        }
        report_failure(self.err, self.tool, &failure).map_err(E::unreported)
    }

    /// Remember the run as failed with nothing to say: a declined `-i`
    /// question, which GNU's tools end with the failing status and no word.
    pub const fn fail_quietly(&mut self) {
        self.clean = false;
    }

    /// Whether nothing has failed.
    #[must_use]
    pub const fn clean(&self) -> bool {
        self.clean
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use core::cell::RefCell;

    #[derive(Default)]
    struct Bytes(RefCell<Vec<u8>>);

    impl Output for Bytes {
        fn write_all(&self, bytes: &[u8]) -> Result<(), Errno> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(())
        }
    }

    #[test]
    fn a_sink_with_no_advisory_consumer_drops_the_record_and_keeps_its_output() {
        let sink = Bytes::default();
        let out: &dyn Output = &sink;
        out.write_all(b"row\n")
            .expect("a capturing sink takes every byte");
        out.info(b"{\"kind\":\"omission\"}\n");
        assert_eq!(sink.0.borrow().as_slice(), b"row\n");
    }

    #[test]
    fn a_sink_is_no_terminal_unless_it_says_so() {
        assert!(!Bytes::default().at_terminal());
    }

    #[test]
    fn a_failure_is_named_under_the_tool_s_word_in_one_line() {
        let sink = Bytes::default();
        report_failure(&sink, "rm", "cannot remove 'x': not found").expect("captured");
        assert_eq!(
            sink.0.borrow().as_slice(),
            b"rm: cannot remove 'x': not found\n"
        );
    }

    #[derive(Debug, PartialEq)]
    enum Trial {
        Missing,
        Usage,
        Stream(Errno),
    }

    impl core::fmt::Display for Trial {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str(match self {
                Self::Missing => "cannot access 'x': not found",
                Self::Usage => "invalid usage",
                Self::Stream(_) => "write error",
            })
        }
    }

    impl Failure for Trial {
        fn is_fatal(&self) -> bool {
            !matches!(self, Self::Missing)
        }

        fn unreported(errno: Errno) -> Self {
            Self::Stream(errno)
        }
    }

    struct Refusing;

    impl Output for Refusing {
        fn write_all(&self, _: &[u8]) -> Result<(), Errno> {
            Err(Errno::PermissionDenied)
        }
    }

    #[test]
    fn a_carried_failure_is_reported_and_remembered_and_a_fatal_one_returned() {
        let sink = Bytes::default();
        let mut carried = Carried::new(&sink, "tool");
        assert_eq!(carried.absorb(Ok::<u8, Trial>(7)), Ok(Some(7)));
        assert!(carried.clean());
        assert_eq!(carried.absorb(Err::<u8, _>(Trial::Missing)), Ok(None));
        assert!(!carried.clean());
        assert_eq!(
            carried.absorb(Err::<u8, _>(Trial::Usage)),
            Err(Trial::Usage)
        );
        assert_eq!(carried.carry(Trial::Usage), Err(Trial::Usage));
        assert_eq!(carried.carry(Trial::Missing), Ok(()));
        assert_eq!(
            sink.0.borrow().as_slice(),
            b"tool: cannot access 'x': not found\ntool: cannot access 'x': not found\n",
            "a fatal failure is the caller's to report"
        );
    }

    #[test]
    fn a_silenced_run_reports_nothing_yet_ends_failed() {
        let sink = Bytes::default();
        let mut carried = Carried::new(&sink, "tool").silenced(true);
        assert_eq!(carried.absorb(Err::<(), _>(Trial::Missing)), Ok(None));
        assert!(!carried.clean());
        assert!(sink.0.borrow().is_empty());
    }

    #[test]
    fn a_quiet_failure_ends_the_run_failed_and_says_nothing() {
        let sink = Bytes::default();
        let mut carried = Carried::new(&sink, "tool");
        carried.fail_quietly();
        assert!(!carried.clean());
        assert!(sink.0.borrow().is_empty());
    }

    #[test]
    fn a_refused_report_ends_the_run_with_the_stream_s_reason() {
        let mut carried = Carried::new(&Refusing, "tool");
        assert_eq!(
            carried.absorb(Err::<(), _>(Trial::Missing)),
            Err(Trial::Stream(Errno::PermissionDenied))
        );
    }
}
