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
}
