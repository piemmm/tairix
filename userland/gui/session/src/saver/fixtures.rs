//! What the screensaver's tests host a saver over: its pipes and region held
//! in memory, a record of what it asked of the workers, and the log it
//! wrote.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::font_ipc::FontRequest;
use tairix_abi::saver_ipc::{HostMessage, SaverMessage};
use tairix_abi::Errno;
use tairix_log::{Event, EventId, FieldValue, Sink};
use tairix_sandbox::{SessionDescriptors, SessionTransport};

use super::{Picture, SaverErrands, SaverLauncher, SaverMemory};

/// What passed down a saver's pipes, held in memory.
#[derive(Default)]
pub(crate) struct Wire {
    /// What the host sent.
    pub(crate) sent: Vec<u8>,
    /// What the saver is to say.
    pub(crate) said: VecDeque<u8>,
    /// Whether the saver closed its end.
    pub(crate) closed: bool,
}

/// The host's end of a saver's pipes.
#[derive(Clone, Default)]
pub(crate) struct Pipes(pub(crate) Rc<RefCell<Wire>>);

impl SessionTransport for Pipes {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
        let mut wire = self.0.borrow_mut();
        if wire.said.is_empty() {
            return if wire.closed {
                Ok(0)
            } else {
                Err(Errno::WouldBlock)
            };
        }
        let n = buf.len().min(wire.said.len());
        for (slot, byte) in buf.iter_mut().zip(wire.said.drain(..n)) {
            *slot = byte;
        }
        Ok(n)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
        self.0.borrow_mut().sent.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn descriptors(&self) -> Option<SessionDescriptors> {
        None
    }

    fn dispose(self) -> Option<i32> {
        Some(0)
    }
}

impl Pipes {
    /// Have the saver say `message`.
    pub(crate) fn say(&self, message: &SaverMessage) {
        let mut out = [0u8; tairix_abi::saver_ipc::SAVER_MESSAGE_MAX];
        let len = message.encode(&mut out).expect("encodes");
        let mut wire = self.0.borrow_mut();
        wire.said
            .extend(u32::try_from(len).expect("small").to_le_bytes());
        wire.said.extend(&out[..len]);
    }

    /// Have the saver say bytes that are no message.
    pub(crate) fn garble(&self) {
        let mut wire = self.0.borrow_mut();
        wire.said.extend(1u32.to_le_bytes());
        wire.said.push_back(0x7F);
    }

    /// Every message the host sent, each checked to decode.
    fn sent(&self) -> Vec<Vec<u8>> {
        let wire = self.0.borrow();
        let mut at = 0;
        let mut sent = Vec::new();
        while at + 4 <= wire.sent.len() {
            let len = u32::from_le_bytes(wire.sent[at..at + 4].try_into().expect("four")) as usize;
            let payload = wire.sent[at + 4..at + 4 + len].to_vec();
            HostMessage::decode(&payload).expect("a message");
            sent.push(payload);
            at += 4 + len;
        }
        sent
    }

    /// The operation byte of every message the host sent.
    pub(crate) fn heard(&self) -> Vec<u8> {
        self.sent().iter().map(|payload| payload[0]).collect()
    }

    /// The last message the host sent.
    pub(crate) fn last(&self) -> Vec<u8> {
        self.sent().pop().unwrap_or_default()
    }
}

/// A region held on the heap.
pub(crate) struct Heap(pub(crate) Vec<u8>);

impl SaverMemory for Heap {
    fn bytes(&self) -> &[u8] {
        &self.0
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

/// What a saver asked of the workers.
#[derive(Default)]
pub(crate) struct Errands {
    pub(crate) fonts: Vec<[u8; FontRequest::WIRE_LEN]>,
    pub(crate) kept: Vec<Picture>,
    pub(crate) slides: Vec<u16>,
}

impl SaverErrands for Errands {
    fn relay_font(&mut self, request: [u8; FontRequest::WIRE_LEN]) {
        self.fonts.push(request);
    }

    fn keep(&mut self, picture: Picture) {
        self.kept.push(picture);
    }

    fn want_slide(&mut self, index: u16) {
        self.slides.push(index);
    }
}

/// A launcher handing out pipes held in memory, each saver's kept for the
/// test, or refusing with `refuse`.
#[derive(Default)]
pub(crate) struct Launches {
    pub(crate) pipes: Vec<Pipes>,
    pub(crate) entries: Vec<alloc::string::String>,
    pub(crate) ceilings: Vec<(u64, u64)>,
    pub(crate) refuse: Option<Errno>,
}

impl SaverLauncher for Launches {
    type Transport = Pipes;

    fn launch(
        &mut self,
        entry: &str,
        len: usize,
        ceilings: (u64, u64),
    ) -> Result<(Pipes, Box<dyn SaverMemory>), Errno> {
        if let Some(errno) = self.refuse {
            return Err(errno);
        }
        let pipes = Pipes::default();
        self.pipes.push(pipes.clone());
        self.entries.push(alloc::string::String::from(entry));
        self.ceilings.push(ceilings);
        Ok((pipes, Box::new(Heap(alloc::vec![0; len]))))
    }
}

/// One record's fields as a log holds them, each key with its value written.
pub(crate) type Fields = Vec<(String, String)>;

/// A log keeping every record handed to it, shared by its clones: each
/// record's id and its fields, the text ones as written.
#[derive(Clone, Default)]
pub(crate) struct Records(Rc<RefCell<Vec<(EventId, Fields)>>>);

impl Records {
    /// The fields of every record of `id`, in the order written.
    pub(crate) fn of(&self, id: EventId) -> Vec<Fields> {
        self.0
            .borrow()
            .iter()
            .filter(|(at, _)| *at == id)
            .map(|(_, fields)| fields.clone())
            .collect()
    }
}

impl Sink for Records {
    fn write_event(&self, event: &Event<'_>) {
        let fields = event
            .fields
            .iter()
            .map(|field| {
                let value = match field.value {
                    FieldValue::Str(text) => String::from(text),
                    FieldValue::Bool(on) => on.to_string(),
                    other => alloc::format!("{other:?}"),
                };
                (String::from(field.key), value)
            })
            .collect();
        self.0.borrow_mut().push((event.id, fields));
    }
}
