//! The saver's end of its two pipes, shared by the main loop and the font
//! relay.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::font_ipc::FontRequest;
use tairix_abi::saver_ipc::{HostMessage, SaverMessage, SAVER_MESSAGE_MAX};
use tairix_abi::Errno;
use tairix_font::FontTransport;
use tairix_rt::sync::Mutex;
use tairix_sandbox::proto::{recv_frame_into, send_frame, Channel, ProtoError};

/// The most messages held for the main loop while a font call waits for its
/// reply: the host sends feeds on their own schedule, and a font round trip is
/// short, so a deeper queue is a host that has stopped answering.
const HELD_MAX: usize = 32;

/// The pipes, and the messages a font call read past.
pub(crate) struct Link<C> {
    channel: C,
    held: VecDeque<Vec<u8>>,
    /// What a font call reads into, kept from one call to the next.
    reading: Vec<u8>,
    out: [u8; SAVER_MESSAGE_MAX],
    broken: Option<Errno>,
}

impl<C: Channel> Link<C> {
    pub(crate) fn new(channel: C) -> Self {
        Self {
            channel,
            held: VecDeque::new(),
            reading: Vec::new(),
            out: [0; SAVER_MESSAGE_MAX],
            broken: None,
        }
    }

    /// Put the next message for the main loop in `into`, reusing its
    /// allocation: one a font call held, else the next read; `false` once the
    /// host has closed its pipe.
    ///
    /// # Errors
    ///
    /// The channel's failure, or what broke the conversation during a font
    /// call.
    pub(crate) fn next_into(&mut self, into: &mut Vec<u8>) -> Result<bool, Errno> {
        if let Some(held) = self.held.pop_front() {
            *into = held;
            return Ok(true);
        }
        if let Some(broken) = self.broken {
            return Err(broken);
        }
        recv_frame_into(&mut self.channel, into).map_err(errno_of)
    }

    /// Send `message`.
    ///
    /// # Errors
    ///
    /// The channel's failure.
    pub(crate) fn send(&mut self, message: &SaverMessage) -> Result<(), Errno> {
        let len = message.encode(&mut self.out)?;
        send_frame(&mut self.channel, &self.out[..len]).map_err(errno_of)
    }

    /// Make the font call `request` and wait for its reply, copying it into
    /// `reply`; whatever else arrives meanwhile is held for the main loop.
    ///
    /// A host that breaks the protocol meanwhile ends the conversation: the
    /// main loop sees it at its next message.
    fn font_call(
        &mut self,
        request: [u8; FontRequest::WIRE_LEN],
        reply: &mut [u8],
    ) -> Result<usize, Errno> {
        if let Some(broken) = self.broken {
            return Err(broken);
        }
        self.send(&SaverMessage::FontCall(request))?;
        loop {
            let mut frame = core::mem::take(&mut self.reading);
            match recv_frame_into(&mut self.channel, &mut frame).map_err(errno_of) {
                Ok(true) => {}
                Ok(false) => return Err(self.break_with(Errno::BrokenPipe)),
                Err(errno) => return Err(self.break_with(errno)),
            }
            let answer = match HostMessage::decode(&frame) {
                Ok(HostMessage::FontReply(answer)) => Some(answer.and_then(|bytes| {
                    let into = reply.get_mut(..bytes.len()).ok_or(Errno::BufferTooSmall)?;
                    into.copy_from_slice(bytes);
                    Ok(bytes.len())
                })),
                Ok(_) => None,
                Err(errno) => return Err(self.break_with(errno)),
            };
            match answer {
                Some(answer) => {
                    self.reading = frame;
                    return answer;
                }
                None if self.held.len() < HELD_MAX => self.held.push_back(frame),
                None => return Err(self.break_with(Errno::LimitExceeded)),
            }
        }
    }

    fn break_with(&mut self, errno: Errno) -> Errno {
        self.broken = Some(errno);
        errno
    }
}

/// The font service, reached through the host: lib/font's transport for a
/// process that may call no endpoint of its own.
pub(crate) struct FontRelay<C> {
    pub(crate) link: Arc<Mutex<Link<C>>>,
}

impl<C: Channel + Send> FontTransport for FontRelay<C> {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        let request = request.try_into().map_err(|_| Errno::LengthOutOfRange)?;
        self.link.lock().font_call(request, reply)
    }
}

fn errno_of(error: ProtoError) -> Errno {
    match error {
        ProtoError::PeerClosed => Errno::BrokenPipe,
        ProtoError::Channel(errno) => errno,
        ProtoError::Oversize => Errno::MessageTooLarge,
        ProtoError::OutOfMemory => Errno::OutOfMemory,
    }
}
