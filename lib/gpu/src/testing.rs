//! A software device served on a channel in this process, each request run
//! and answered on the thread that sends it: what the served device's tests
//! and fuzz harness drive as client processes would, journalling every
//! request a connected client sends so it can be sent again, changed.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tairix_abi::time::MonotonicClock;
use tairix_abi::{Errno, ProcId, PROC_ID_LEN};
use tairix_parallel::JobRunner;
use tairix_rt::sync::Mutex;

use crate::device::{Device, Shared};
use crate::remote::{Link, Remote};
use crate::serve::{Channel, Region, Server};
use crate::software::{info, software_shared, Software};
use crate::wire::ops::ExportToken;
use crate::{DeviceInfo, Error, NoThreads, QueueFamily};

/// A clock that moves only when the harness moves it, and by its step at each
/// reading once given one: a wait's timeout passes when the caller says, and
/// work that would run on stops at its deadline after a bounded count of
/// readings, the same count however often it is run.
#[derive(Default)]
struct Manual {
    now: AtomicU64,
    step: AtomicU64,
}

impl MonotonicClock for Manual {
    fn now_ns(&self) -> u64 {
        let step = self.step.load(Ordering::SeqCst);
        match self
            .now
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |now| {
                Some(now.saturating_add(step))
            }) {
            Ok(now) | Err(now) => now,
        }
    }
}

/// The software device's statement for a machine of `memory` bytes, `second`
/// stated beside its own family where given: an engine its one worker runs as
/// it runs the first, so a harness holds a copy engine's rules on a device
/// that keeps the texels it moves.
fn stating(memory: u64, second: Option<QueueFamily>) -> Result<DeviceInfo, Error> {
    let mut stated = info(memory)?;
    if let Some(second) = second {
        stated
            .queue_families
            .try_push(second)
            .map_err(|_| Error::Malformed)?;
    }
    Ok(stated)
}

/// The software device of a machine of `memory` bytes, its work shared across
/// `runner` and run on the thread that readies it, `second` stated beside its
/// own family where given.
///
/// # Errors
///
/// The software device's refusal, or [`Error::Malformed`] for a family no
/// device states.
pub fn software(
    memory: u64,
    runner: Arc<dyn JobRunner + Send>,
    second: Option<QueueFamily>,
) -> Result<Device, Error> {
    let clock = Arc::new(Manual::default());
    software_shared(stating(memory, second)?, clock, runner, &NoThreads).map(Device::local)
}

/// One request as a client sent it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Step {
    /// The client, by the byte its process id repeats.
    pub proc: u8,
    /// What it wrote into its region before sending it, in order: where,
    /// and the bytes.
    pub writes: Vec<(usize, Vec<u8>)>,
    /// The request frame.
    pub frame: Vec<u8>,
    /// The reply it was answered.
    pub reply: Vec<u8>,
}

/// A region's bytes, shared by its client and the runtime.
pub(crate) struct Bytes(Mutex<Vec<u8>>);

impl Bytes {
    pub(crate) fn zeroed(len: usize) -> Self {
        Self(Mutex::new(alloc::vec![0; len]))
    }
}

impl Region for Bytes {
    fn len(&self) -> usize {
        self.0.lock().len()
    }

    fn read(&self, at: usize, out: &mut [u8]) {
        if let Some(bytes) = self.0.lock().get(at..at.saturating_add(out.len())) {
            out.copy_from_slice(bytes);
        }
    }

    fn write(&self, at: usize, data: &[u8]) {
        if let Some(bytes) = self.0.lock().get_mut(at..at.saturating_add(data.len())) {
            bytes.copy_from_slice(data);
        }
    }

    fn lend(
        &self,
        at: usize,
        len: usize,
        copy: &mut dyn FnMut(&[u8]) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let bytes = self.0.lock();
        let end = at.checked_add(len).ok_or(Error::OutOfRange)?;
        copy(bytes.get(at..end).ok_or(Error::OutOfRange)?)
    }

    fn lend_mut(
        &self,
        at: usize,
        len: usize,
        copy: &mut dyn FnMut(&mut [u8]) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let mut bytes = self.0.lock();
        let end = at.checked_add(len).ok_or(Error::OutOfRange)?;
        copy(bytes.get_mut(at..end).ok_or(Error::OutOfRange)?)
    }
}

/// The loopback channel: replies collected for the sender to take, regions
/// kept by grant for the process that granted each.
#[derive(Default)]
struct Line {
    replies: Mutex<Vec<(u64, Vec<u8>)>>,
    /// Grant `n` is entry `n - 1`.
    regions: Mutex<Vec<(ProcId, Arc<Bytes>)>>,
    tokens: AtomicU64,
    woken: AtomicBool,
}

impl Line {
    fn region(&self, grant: u64) -> Option<(ProcId, Arc<Bytes>)> {
        let index = usize::try_from(grant.checked_sub(1)?).ok()?;
        self.regions
            .lock()
            .get(index)
            .map(|(owner, bytes)| (*owner, Arc::clone(bytes)))
    }
}

impl Channel for Line {
    fn reply(&self, ticket: u64, frame: &[u8]) {
        self.replies.lock().push((ticket, frame.to_vec()));
    }

    fn map(&self, client: ProcId, grant: u64, min_len: usize) -> Result<Arc<dyn Region>, Errno> {
        let (owner, bytes) = self.region(grant).ok_or(Errno::NotFound)?;
        if owner != client {
            return Err(Errno::PermissionDenied);
        }
        if bytes.len() < min_len {
            return Err(Errno::LengthOutOfRange);
        }
        Ok(bytes as Arc<dyn Region>)
    }

    fn watch(&self, _: ProcId) -> Result<(), Errno> {
        Ok(())
    }

    fn unwatch(&self, _: ProcId) {}

    // Counted, so a journal sent again names the tokens it named first.
    fn token(&self) -> Result<ExportToken, Errno> {
        let mut token = ExportToken::default();
        let count = self.tokens.fetch_add(1, Ordering::SeqCst) + 1;
        token[..8].copy_from_slice(&count.to_le_bytes());
        Ok(token)
    }

    fn wake(&self) {
        self.woken.store(true, Ordering::SeqCst);
    }
}

/// What one request was answered: its ticket, and every reply sent while it
/// was served — its own, unless it is a wait the runtime holds, and those of
/// the waits it settled.
#[derive(Debug)]
pub struct Answered {
    /// The request's ticket.
    pub ticket: u64,
    /// The replies sent, each with the ticket it answers.
    pub replies: Vec<(u64, Vec<u8>)>,
}

/// A software device served on a loopback channel, its runtime running every
/// request on the thread that sends it.
pub struct Loopback {
    shared: Arc<Shared<Software>>,
    server: Arc<Server>,
    line: Arc<Line>,
    clock: Arc<Manual>,
    tickets: AtomicU64,
    journal: Mutex<Vec<Step>>,
}

impl Loopback {
    /// The software device of a machine of `memory` bytes, its work shared
    /// across `runner` and run on the thread that readies it, `second` stated
    /// beside its own family where given, served.
    ///
    /// # Errors
    ///
    /// The software device's refusal.
    pub fn new(
        memory: u64,
        runner: Arc<dyn JobRunner + Send>,
        second: Option<QueueFamily>,
    ) -> Result<Arc<Self>, Error> {
        let clock = Arc::new(Manual::default());
        let shared = software_shared(
            stating(memory, second)?,
            Arc::clone(&clock) as Arc<dyn MonotonicClock + Send + Sync>,
            runner,
            &NoThreads,
        )?;
        let line = Arc::new(Line::default());
        let server = Server::new(
            Arc::clone(&shared) as Arc<dyn crate::host::Served>,
            Arc::clone(&line) as Arc<dyn Channel>,
        );
        Ok(Arc::new(Self {
            shared,
            server,
            line,
            clock,
            tickets: AtomicU64::new(0),
            journal: Mutex::new(Vec::new()),
        }))
    }

    /// The device served, as its driver holds it.
    #[must_use]
    pub fn device(&self) -> Device {
        Device::local(Arc::clone(&self.shared))
    }

    /// A region of `len` bytes, granted by process `proc`: its grant, the
    /// next in turn from 1.
    pub fn grant(&self, proc: u8, len: usize) -> u64 {
        let mut regions = self.line.regions.lock();
        regions.push((process(proc), Arc::new(Bytes::zeroed(len))));
        regions.len() as u64
    }

    /// Copy `data` into the region granted as `grant` at `at`, as its client
    /// would; nothing where it does not fit.
    pub fn write_region(&self, grant: u64, at: usize, data: &[u8]) {
        if let Some((_, bytes)) = self.line.region(grant) {
            bytes.write(at, data);
        }
    }

    /// Read `out.len()` bytes at `at` of the region granted as `grant`, as
    /// the server's mapping of it holds them.
    pub fn read_region(&self, grant: u64, at: usize, out: &mut [u8]) {
        if let Some((_, bytes)) = self.line.region(grant) {
            bytes.read(at, out);
        }
    }

    /// How many mappings the server keeps of the region granted as `grant`.
    #[must_use]
    pub fn region_holders(&self, grant: u64) -> usize {
        // The loopback's own and the one looked up here are no mappings.
        self.line
            .region(grant)
            .map_or(0, |(_, bytes)| Arc::strong_count(&bytes).saturating_sub(2))
    }

    /// Process `proc`, connected through a region of `len` bytes it grants,
    /// each request it sends journalled.
    ///
    /// # Errors
    ///
    /// The runtime's refusal of its `Open`.
    pub fn connect(self: &Arc<Self>, proc: u8, len: usize) -> Result<Device, Errno> {
        let grant = self.grant(proc, len);
        let link = Client {
            loopback: Arc::clone(self),
            proc,
            grant,
            len,
            writes: Mutex::new(Vec::new()),
        };
        let remote = Remote::open(Box::new(link), grant)?;
        Ok(Device {
            host: Arc::new(remote),
            served: None,
        })
    }

    /// Serve `frame` from process `proc`.
    pub fn serve(&self, proc: u8, frame: &[u8]) -> Answered {
        let ticket = self.tickets.fetch_add(1, Ordering::SeqCst) + 1;
        self.server.serve(ticket, process(proc), frame, 0);
        Answered {
            ticket,
            replies: self.settle(),
        }
    }

    /// Move the clock to the soonest a wait times out or a job overruns, and
    /// answer what fell due: the replies sent, or `None` where nothing is
    /// due at all.
    pub fn advance(&self) -> Option<Vec<(u64, Vec<u8>)>> {
        let due = self.server.deadline()?;
        self.clock.now.fetch_max(due, Ordering::SeqCst);
        self.server.expire(self.clock.now_ns());
        Some(self.settle())
    }

    /// Move the clock on by `step` nanoseconds at every reading from now on,
    /// so work a request makes endless stops at its deadline.
    pub fn step_clock(&self, step: u64) {
        self.clock.step.store(step, Ordering::SeqCst);
    }

    /// Process `proc` exits.
    pub fn exit(&self, proc: u8) -> Vec<(u64, Vec<u8>)> {
        self.server.client_exited(process(proc));
        self.settle()
    }

    /// Every request a connected client sent, in order, taken.
    pub fn take_journal(&self) -> Vec<Step> {
        core::mem::take(&mut *self.journal.lock())
    }

    /// Who granted each region, and its bytes, in grant order.
    #[must_use]
    pub fn grants(&self) -> Vec<(u8, usize)> {
        self.line
            .regions
            .lock()
            .iter()
            .map(|(owner, bytes)| (owner.as_bytes()[0], bytes.len()))
            .collect()
    }

    /// What the device's heap has charged.
    #[must_use]
    pub fn heap_taken(&self) -> u64 {
        self.shared.model.lock().work_heap_taken()
    }

    /// Answer the waits what the device did meanwhile settled, and take
    /// every reply sent.
    fn settle(&self) -> Vec<(u64, Vec<u8>)> {
        if self.line.woken.swap(false, Ordering::SeqCst) {
            self.server.moved();
        }
        core::mem::take(&mut *self.line.replies.lock())
    }
}

fn process(proc: u8) -> ProcId {
    ProcId::from_raw([proc; PROC_ID_LEN])
}

/// A client's link to the loopback, journalling what it sends.
struct Client {
    loopback: Arc<Loopback>,
    proc: u8,
    grant: u64,
    len: usize,
    writes: Mutex<Vec<(usize, Vec<u8>)>>,
}

impl Link for Client {
    fn call(&self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        let writes = core::mem::take(&mut *self.writes.lock());
        let served = self.loopback.serve(self.proc, request);
        let mut replies = served.replies;
        // A wait held: the client blocks until it is answered, so time moves
        // on to whatever answers it first.
        let answer = loop {
            if let Some(at) = replies
                .iter()
                .position(|(ticket, _)| *ticket == served.ticket)
            {
                break replies.swap_remove(at).1;
            }
            replies = self.loopback.advance().ok_or(Errno::Deadlock)?;
        };
        let out = reply.get_mut(..answer.len()).ok_or(Errno::BufferTooSmall)?;
        out.copy_from_slice(&answer);
        self.loopback.journal.lock().push(Step {
            proc: self.proc,
            writes,
            frame: request.to_vec(),
            reply: answer,
        });
        Ok(out.len())
    }

    fn region_len(&self) -> usize {
        self.len
    }

    fn write_region(&self, at: usize, data: &[u8]) {
        self.writes.lock().push((at, data.to_vec()));
        self.loopback.write_region(self.grant, at, data);
    }

    fn read_region(&self, at: usize, out: &mut [u8]) {
        if let Some((_, bytes)) = self.loopback.line.region(self.grant) {
            bytes.read(at, out);
        }
    }

    // The loopback's grants are made by the test, which names one as the
    // region it attaches.
    fn grant(&self, region: u64) -> Result<u64, Errno> {
        Ok(region)
    }
}
