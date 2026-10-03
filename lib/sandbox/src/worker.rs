//! The worker side of the sandbox seam: the serve loop a sandboxed
//! process runs.
//!
//! A worker is the parent program's own binary started in a worker role
//! inside the kernel sandbox spawn mode (`docs/src/security/sandbox.md`):
//! it holds exactly two pipe ends and the self-scoped allow-list syscalls,
//! nothing else. Its whole life is [`serve`]: read a request frame, hand
//! the payload to the [`Service`], write the reply frame, repeat until the
//! parent closes the request stream.
//!
//! A [`Service`] is **total**: it must return a reply for every payload,
//! encoding "that request is malformed" as a typed error *reply* rather
//! than failing the loop. Panics are not a control path — the decoders a
//! service runs are panic-free by contract — but if a service does crash,
//! the kernel contains it to this process and the parent's seam turns the
//! dead stream into a typed error and a replacement worker
//! ([`crate::host::ParserSandbox`]).

use alloc::vec::Vec;

use crate::proto::{recv_frame_into, send_frame, Channel, ProtoError};

/// One request/reply protocol a worker can serve.
///
/// The payload bytes are opaque to the framing; the service defines their
/// meaning (`crate::decode::DecodeService` is the executable-inspection
/// one). The reply must fit [`crate::proto::MAX_FRAME`]; a service that
/// respects its own documented reply caps stays well inside it.
pub trait Service {
    /// Produce the reply payload for one request payload.
    fn handle(&mut self, request: &[u8]) -> Vec<u8>;
}

/// How a serve loop ended.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ServeEnd {
    /// The parent closed the request stream on a frame boundary: the
    /// conversation is over and the worker exits cleanly.
    Finished,
    /// The **service** closed the session (`crate::session::SessionStep`).
    /// Only [`crate::session::serve_session`] produces this; a one-shot
    /// [`Service`] cannot end its own loop, so [`serve`] never does.
    Ended,
    /// The transport failed. The worker can only exit; the
    /// parent-side seam observes the dead stream and contains it.
    Failed(ProtoError),
}

impl ServeEnd {
    /// How the worker process ends.
    #[must_use]
    pub const fn exit(&self) -> WorkerExit {
        match self {
            Self::Finished | Self::Ended => WorkerExit::Finished,
            Self::Failed(ProtoError::PeerClosed) => WorkerExit::PeerClosed,
            Self::Failed(ProtoError::OutOfMemory) => WorkerExit::OutOfMemory,
            Self::Failed(ProtoError::Oversize) => WorkerExit::Oversize,
            Self::Failed(ProtoError::Channel(_)) => WorkerExit::Transport,
        }
    }

    /// The exit status the worker process ends with.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        self.exit().code()
    }
}

/// Why a worker process ended, carried as its exit status.
///
/// Once its transport has failed the worker has no other way to say why —
/// its error and information streams are closed in the sandbox — so the
/// status is the diagnosis, and the parent's containment log states it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WorkerExit {
    /// The parent closed the conversation on a frame boundary.
    Finished,
    /// The parent's stream ended inside a frame.
    PeerClosed,
    /// The room to receive a request could not be reserved.
    OutOfMemory,
    /// The parent declared a request larger than a frame may carry.
    Oversize,
    /// The transport failed with an error of its own.
    Transport,
}

impl WorkerExit {
    /// The exit status this ending is carried as.
    #[must_use]
    pub const fn code(self) -> i32 {
        match self {
            Self::Finished => 0,
            Self::PeerClosed => 1,
            Self::OutOfMemory => 2,
            Self::Oversize => 3,
            Self::Transport => 4,
        }
    }

    /// The ending a worker's exit status `code` carries, if it is one.
    #[must_use]
    pub const fn from_code(code: i32) -> Option<Self> {
        match code {
            0 => Some(Self::Finished),
            1 => Some(Self::PeerClosed),
            2 => Some(Self::OutOfMemory),
            3 => Some(Self::Oversize),
            4 => Some(Self::Transport),
            _ => None,
        }
    }
}

/// Serve requests until the request stream closes.
///
/// Every iteration is strictly request → reply, so the parent's
/// one-outstanding-request discipline holds by construction.
pub fn serve<C: Channel, S: Service>(chan: &mut C, service: &mut S) -> ServeEnd {
    let mut request = Vec::new();
    loop {
        match recv_frame_into(chan, &mut request) {
            Ok(true) => {}
            Ok(false) => return ServeEnd::Finished,
            Err(err) => return ServeEnd::Failed(err),
        }
        let reply = service.handle(&request);
        if let Err(err) = send_frame(chan, &reply) {
            return ServeEnd::Failed(err);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{serve, ServeEnd, Service, WorkerExit};
    use crate::proto::{recv_frame, send_frame, Channel, ProtoError, MAX_FRAME};
    use alloc::vec;
    use alloc::vec::Vec;
    use tairix_abi::Errno;

    /// A service that echoes each request back reversed.
    struct Reverser;

    impl Service for Reverser {
        fn handle(&mut self, request: &[u8]) -> Vec<u8> {
            let mut out = request.to_vec();
            out.reverse();
            out
        }
    }

    /// Loopback whose input holds the scripted parent->worker bytes and
    /// whose output collects the worker->parent bytes.
    struct Loopback {
        input: Vec<u8>,
        at: usize,
        output: Vec<u8>,
    }

    impl Channel for Loopback {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
            if self.at == self.input.len() || buf.is_empty() {
                return Ok(0);
            }
            let take = buf.len().min(self.input.len() - self.at);
            buf[..take].copy_from_slice(&self.input[self.at..self.at + take]);
            self.at += take;
            Ok(take)
        }

        fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }
    }

    #[test]
    fn serves_each_request_in_order_and_finishes_on_boundary_eof() {
        let mut script = Loopback {
            input: Vec::new(),
            at: 0,
            output: Vec::new(),
        };
        send_frame(&mut script, b"abc").expect("frame");
        send_frame(&mut script, b"12345").expect("frame");
        let mut chan = Loopback {
            input: script.output,
            at: 0,
            output: Vec::new(),
        };

        assert_eq!(serve(&mut chan, &mut Reverser), ServeEnd::Finished);

        let mut replies = Loopback {
            input: chan.output,
            at: 0,
            output: Vec::new(),
        };
        assert_eq!(
            recv_frame(&mut replies).expect("reply"),
            Some(b"cba".to_vec())
        );
        assert_eq!(
            recv_frame(&mut replies).expect("reply"),
            Some(b"54321".to_vec())
        );
        assert_eq!(recv_frame(&mut replies), Ok(None));
    }

    #[test]
    fn a_parent_that_dies_mid_frame_fails_the_loop_typed() {
        // A header declaring three bytes, then EOF.
        let mut chan = Loopback {
            input: 3u32.to_le_bytes().to_vec(),
            at: 0,
            output: Vec::new(),
        };
        assert_eq!(
            serve(&mut chan, &mut Reverser),
            ServeEnd::Failed(ProtoError::PeerClosed)
        );
    }

    #[test]
    fn an_oversize_request_declaration_fails_the_loop_before_allocation() {
        let declared = u32::try_from(MAX_FRAME + 1).expect("fits");
        let mut chan = Loopback {
            input: declared.to_le_bytes().to_vec(),
            at: 0,
            output: Vec::new(),
        };
        assert_eq!(
            serve(&mut chan, &mut Reverser),
            ServeEnd::Failed(ProtoError::Oversize)
        );
    }

    #[test]
    fn an_empty_request_is_served_not_confused_with_eof() {
        let mut script = Loopback {
            input: Vec::new(),
            at: 0,
            output: Vec::new(),
        };
        send_frame(&mut script, b"").expect("frame");
        let mut chan = Loopback {
            input: script.output,
            at: 0,
            output: Vec::new(),
        };
        assert_eq!(serve(&mut chan, &mut Reverser), ServeEnd::Finished);
        let mut replies = Loopback {
            input: chan.output,
            at: 0,
            output: Vec::new(),
        };
        assert_eq!(recv_frame(&mut replies), Ok(Some(vec![])));
    }

    #[test]
    fn a_worker_exits_cleanly_only_when_its_conversation_closed() {
        assert_eq!(ServeEnd::Finished.exit_code(), 0);
        assert_eq!(ServeEnd::Ended.exit_code(), 0);
        assert_ne!(ServeEnd::Failed(ProtoError::PeerClosed).exit_code(), 0);
    }

    #[test]
    fn every_transport_failure_exits_with_a_status_that_names_it() {
        let failures = [
            (ProtoError::PeerClosed, WorkerExit::PeerClosed),
            (ProtoError::OutOfMemory, WorkerExit::OutOfMemory),
            (ProtoError::Oversize, WorkerExit::Oversize),
            (
                ProtoError::Channel(Errno::Interrupted),
                WorkerExit::Transport,
            ),
        ];
        for (failure, exit) in failures {
            let code = ServeEnd::Failed(failure).exit_code();
            assert_eq!(WorkerExit::from_code(code), Some(exit), "{failure:?}");
        }
        // The loader's reserved band and the signal statuses are never mistaken
        // for a worker's own diagnosis.
        assert_eq!(WorkerExit::from_code(tairix_abi::LOAD_OOM), None);
        assert_eq!(
            WorkerExit::from_code(tairix_abi::Signal::Kill.termination_status().unwrap_or(0)),
            None
        );
        assert_eq!(WorkerExit::from_code(tairix_abi::FAULT_EXIT_STATUS), None);
    }
}
