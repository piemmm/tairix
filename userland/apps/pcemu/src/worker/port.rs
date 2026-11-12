//! The worker's end of the session, shared by the run loop and the machine's
//! disks: the link, the media the front-end named, the sectors fetched from
//! them, and the commands that arrived while a disk waited on its answer.
//!
//! A disk asks for what it lacks and waits with the machine stopped inside
//! the access, so emulated time never sees how long the front-end took. While
//! it waits every other command is read and kept, in order, to be applied once
//! the machine is back in the run loop; the window bounds how many there are.

use super::cache::{SectorCache, BLOCK_SECTORS};
use crate::folder::{Op, Refusal, Reply};
use crate::link::{
    Command, Event, Form, CREDITED_MOST, INPUT_WINDOW, MAX_MEDIA, MAX_RUN, TYPE_OVERHEAD,
    TYPE_WINDOW,
};
use crate::storage::{BlockDevice, MediaError, SECTOR};
use crate::time::Nanos;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use tairix_collections::ByteQueue;
use tairix_reclaim::PressureBand;
use tairix_sandbox::link::Waited;
use tairix_sandbox::wire::Writer;
use tairix_sandbox::ProtoError;

/// The worker's end of its session, and the clock its waits are measured by.
pub trait Link {
    /// Lend the next whole command's payload to `take`, making at most one
    /// read that waits at most `wait` (for ever where it is `None`).
    ///
    /// # Errors
    ///
    /// The session's failure; the worker ends on it.
    fn recv<R>(
        &mut self,
        wait: Option<Nanos>,
        take: impl FnOnce(&[u8]) -> R,
    ) -> Result<Waited<R>, ProtoError>;

    /// Queue one event's payload to go with the next flush.
    ///
    /// # Errors
    ///
    /// The session's failure; the worker ends on it.
    fn queue(&mut self, payload: &[u8]) -> Result<(), ProtoError>;

    /// Write every event queued, waiting for room.
    ///
    /// # Errors
    ///
    /// The session's failure; the worker ends on it.
    fn flush(&mut self) -> Result<(), ProtoError>;

    /// The host's monotonic clock.
    fn now(&self) -> Nanos;
}

/// Why the worker stops: the front-end closed the session, the session
/// failed, or the front-end broke the protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// The front-end closed the session.
    Closed,
    /// The session failed.
    Failed(ProtoError),
    /// The front-end sent what the protocol does not allow.
    Violation(&'static str),
    /// The machine could not be built, or built again.
    Refused(crate::link::Refusal),
}

/// A medium the front-end named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Medium {
    id: u16,
    form: Form,
}

/// Bytes of commands kept while a disk waits, each behind its length: a full
/// window of them, and a typing window's text sent a character a piece, the
/// most its pieces take.
const DEFERRED_BYTES: usize =
    INPUT_WINDOW as usize * (2 + CREDITED_MOST) + TYPE_WINDOW * (2 + TYPE_OVERHEAD + 1);

/// Keep `payload` in `deferred` behind its length.
fn keep(deferred: &mut ByteQueue, payload: &[u8]) -> Result<(), &'static str> {
    let len = u16::try_from(payload.len()).map_err(|_| "a command too long to keep")?;
    let slot = deferred
        .append_slot(2 + payload.len())
        .map_err(|_| "a command too long to keep")?;
    slot[..2].copy_from_slice(&len.to_le_bytes());
    slot[2..].copy_from_slice(payload);
    Ok(())
}

/// Where an answer's bytes land: the run a disk fetch brought, and the bytes
/// a folder read did.
struct Lands<'l> {
    run: &'l mut Landed,
    read: &'l mut Vec<u8>,
}

/// A folder's answer as the port holds it once its frame is gone: a read's
/// bytes landed in the port, every other reply whole.
enum Answered {
    Reply(Reply<'static>),
    Read(usize),
}

/// `reply` with nothing borrowed from its frame, a read's bytes copied into
/// `read`; `None` where there was no room for them.
fn detached(reply: Reply<'_>, read: &mut Vec<u8>) -> Option<Answered> {
    match reply.detach() {
        Ok(reply) => Some(Answered::Reply(reply)),
        Err(bytes) => {
            read.clear();
            read.try_reserve(bytes.len()).ok()?;
            read.extend_from_slice(bytes);
            Some(Answered::Read(bytes.len()))
        }
    }
}

/// The run a fetch last brought, which the access that asked for it and the
/// ones after it read, whatever the cache keeps.
struct Landed {
    medium: u16,
    first: u64,
    bytes: Vec<u8>,
}

impl Landed {
    /// Where sector `sector` of `medium` lies in the run, if the run holds it.
    fn at(&self, medium: u16, sector: u64) -> Option<usize> {
        let at = usize::try_from(sector.checked_sub(self.first)?)
            .ok()?
            .checked_mul(SECTOR)?;
        (self.medium == medium && at + SECTOR <= self.bytes.len()).then_some(at)
    }

    fn sector(&self, medium: u16, sector: u64) -> Option<&[u8]> {
        self.at(medium, sector)
            .map(|at| &self.bytes[at..at + SECTOR])
    }
}

/// The worker's end of the session.
pub struct Port<L> {
    link: L,
    /// Set once the session has ended; every access then fails.
    ended: Option<End>,
    media: Vec<Medium>,
    cache: SectorCache,
    landed: Landed,
    /// The bytes the last folder read brought.
    read: Vec<u8>,
    next_request: u32,
    /// Commands kept while a disk waited, each a length then its payload.
    deferred: ByteQueue,
    /// Credited commands kept, and the characters to type kept.
    deferred_count: u32,
    deferred_typed: usize,
    /// Slots the front-end released while a disk waited.
    released: u32,
    /// The latest fill and band reported while a disk waited.
    fill: Option<u32>,
    band: Option<PressureBand>,
    /// Host time spent waiting on answers since the run loop last asked.
    waited: Nanos,
    out: Vec<u8>,
}

impl<L: Link> Port<L> {
    /// The port over `link`, keeping fetched sectors in `cache`.
    ///
    /// # Errors
    ///
    /// `None` when its fixed buffers cannot be had.
    pub fn new(link: L, cache: SectorCache) -> Option<Self> {
        let mut media = Vec::new();
        media.try_reserve_exact(MAX_MEDIA).ok()?;
        let mut landed = Vec::new();
        landed
            .try_reserve_exact(usize::from(MAX_RUN) * SECTOR)
            .ok()?;
        Some(Self {
            link,
            ended: None,
            media,
            cache,
            landed: Landed {
                medium: 0,
                first: 0,
                bytes: landed,
            },
            read: Vec::new(),
            next_request: 0,
            deferred: ByteQueue::committed(DEFERRED_BYTES).ok()?,
            deferred_count: 0,
            deferred_typed: 0,
            released: 0,
            fill: None,
            band: None,
            waited: 0,
            out: Vec::new(),
        })
    }

    /// Why the session ended, if it has.
    pub const fn ended(&self) -> Option<End> {
        self.ended
    }

    /// End the session for `end`, unless it already ended.
    pub fn end(&mut self, end: End) {
        self.ended.get_or_insert(end);
    }

    /// The host's clock.
    pub fn now(&self) -> Nanos {
        self.link.now()
    }

    /// The cache, to rebudget or to give back to pressure.
    pub fn cache(&mut self) -> &mut SectorCache {
        &mut self.cache
    }

    /// Name `medium` as `form`, for the machine's disks to reach; answers
    /// whether there was room, which there always is for the drives a machine
    /// can have.
    pub fn name(&mut self, id: u16, form: Form) -> bool {
        if self.media.len() == MAX_MEDIA {
            return false;
        }
        self.media.push(Medium { id, form });
        true
    }

    /// Bytes of the media named now.
    pub fn media_bytes(&self) -> u64 {
        self.media
            .iter()
            .map(|medium| medium.form.units().saturating_mul(SECTOR as u64))
            .fold(0, u64::saturating_add)
    }

    /// Forget `medium`: nothing more is asked of it.
    pub fn forget(&mut self, id: u16) {
        self.media.retain(|medium| medium.id != id);
        if self.landed.medium == id {
            self.landed.bytes.clear();
        }
    }

    /// Queue `event`, to go with the next [`Self::flush`].
    pub fn send(&mut self, event: &Event<'_>) {
        if self.ended.is_some() {
            return;
        }
        let mut w = Writer::reusing(core::mem::take(&mut self.out));
        event.encode(&mut w);
        let payload = w.finish();
        if let Err(error) = self.link.queue(&payload) {
            self.end(End::Failed(error));
        }
        self.out = payload;
    }

    /// Write every event queued.
    pub fn flush(&mut self) {
        if self.ended.is_some() {
            return;
        }
        if let Err(error) = self.link.flush() {
            self.end(End::Failed(error));
        }
    }

    /// The next command for the run loop, copied into `into`: one kept while
    /// a disk waited, else one read from the link waiting at most `wait`.
    /// Answers whether one came.
    pub fn next(&mut self, wait: Option<Nanos>, into: &mut Vec<u8>) -> bool {
        if let Some(len) = self.deferred_len() {
            into.clear();
            into.extend_from_slice(&self.deferred.pending()[2..2 + len]);
            self.deferred.consume(2 + len);
            match Command::decode(into) {
                Ok(Command::Type(codes)) => self.deferred_typed -= codes.len(),
                _ => self.deferred_count -= 1,
            }
            return true;
        }
        if self.ended.is_some() {
            return false;
        }
        match self.link.recv(wait, |payload| {
            into.clear();
            into.try_reserve(payload.len())
                .map(|()| into.extend_from_slice(payload))
        }) {
            Ok(Waited::Frame(Ok(()))) => true,
            Ok(Waited::Frame(Err(_))) => {
                self.end(End::Failed(ProtoError::OutOfMemory));
                false
            }
            Ok(Waited::Nothing) => false,
            Ok(Waited::Closed) => {
                self.end(End::Closed);
                false
            }
            Err(error) => {
                self.end(End::Failed(error));
                false
            }
        }
    }

    fn deferred_len(&self) -> Option<usize> {
        let pending = self.deferred.pending();
        let head = pending.get(..2)?;
        Some(usize::from(u16::from_le_bytes([head[0], head[1]])))
    }

    /// The slots released, the latest fill and the latest band reported while
    /// a disk waited, each taken.
    pub fn take_waiting_readings(&mut self) -> (u32, Option<u32>, Option<PressureBand>) {
        (
            core::mem::take(&mut self.released),
            self.fill.take(),
            self.band.take(),
        )
    }

    /// Host time spent waiting on answers since the last call.
    pub fn take_waited(&mut self) -> Nanos {
        core::mem::take(&mut self.waited)
    }

    fn medium(&self, id: u16) -> Option<Medium> {
        self.media.iter().copied().find(|medium| medium.id == id)
    }

    /// Read sector `sector` of `medium` into `out`: from the last run fetched
    /// or the cache, else fetched with the run of sectors behind it.
    pub fn read(&mut self, id: u16, sector: u64, out: &mut [u8; SECTOR]) -> Result<(), MediaError> {
        let medium = self.medium(id).ok_or(MediaError::Failed)?;
        let units = medium.form.units();
        if sector >= units {
            return Err(MediaError::OutOfRange);
        }
        if let Some(bytes) = self.landed.sector(id, sector) {
            out.copy_from_slice(bytes);
            return Ok(());
        }
        if self.cache.read(id, sector, out) {
            return Ok(());
        }
        let first = sector - sector % BLOCK_SECTORS;
        self.fetch(id, first, (units - first).min(u64::from(MAX_RUN)))?;
        let Self { landed, cache, .. } = self;
        cache.keep(id, first, &landed.bytes);
        out.copy_from_slice(landed.sector(id, sector).ok_or(MediaError::Failed)?);
        Ok(())
    }

    /// Fetch `count` sectors of `medium` from `first` into the landing run.
    ///
    /// # Errors
    ///
    /// The front-end's answer, or [`MediaError::Failed`] once the session has
    /// ended.
    pub fn fetch(&mut self, id: u16, first: u64, count: u64) -> Result<(), MediaError> {
        let count = u16::try_from(count).map_err(|_| MediaError::Failed)?;
        let request = self.request(&Event::Fetch {
            request: self.next_request,
            medium: id,
            first,
            count,
        })?;
        let wanted = usize::from(count) * SECTOR;
        self.await_answer(
            request,
            Err(MediaError::Failed),
            |command, lands| match command {
                Command::Fetched {
                    answer: Ok(data), ..
                } if data.len() == wanted => {
                    lands.run.bytes.clear();
                    lands.run.bytes.extend_from_slice(data);
                    (lands.run.medium, lands.run.first) = (id, first);
                    Some(Ok(()))
                }
                Command::Fetched {
                    answer: Err(error), ..
                } => Some(Err(error)),
                _ => None,
            },
        )
    }

    /// Recall `out.len()` bytes, at most [`crate::link::KEPT_PIECE`], of the
    /// snapshot being resumed, from `offset`.
    ///
    /// # Errors
    ///
    /// [`MediaError::Failed`] once the session has ended, or for an answer of
    /// another size.
    pub fn recall(&mut self, offset: u32, out: &mut [u8]) -> Result<(), MediaError> {
        let len = u32::try_from(out.len()).map_err(|_| MediaError::Failed)?;
        let request = self.request(&Event::Recall {
            request: self.next_request,
            offset,
            len,
        })?;
        self.await_answer(
            request,
            Err(MediaError::Failed),
            |command, _| match command {
                Command::Recalled { bytes, .. } if bytes.len() == out.len() => {
                    out.copy_from_slice(bytes);
                    Some(Ok(()))
                }
                _ => None,
            },
        )
    }

    /// Ask share `slot` `op` and wait for the folder's answer, the machine
    /// stopped meanwhile; a read's bytes are the port's until the next.
    ///
    /// # Errors
    ///
    /// The folder's refusal; [`Refusal::Gone`] once the session has ended.
    pub fn folder(&mut self, slot: u8, op: &Op<'_>) -> Result<Reply<'_>, Refusal> {
        let request = self
            .request(&Event::Folder {
                request: self.next_request,
                slot,
                op: *op,
            })
            .map_err(|_| Refusal::Gone)?;
        let answered =
            self.await_answer(
                request,
                Err(Refusal::Gone),
                |command, lands| match command {
                    Command::Foldered { answer, .. } => Some(match answer {
                        Ok(reply) => Ok(detached(reply, lands.read)?),
                        Err(refusal) => Err(refusal),
                    }),
                    _ => None,
                },
            )?;
        Ok(match answered {
            Answered::Reply(reply) => reply,
            Answered::Read(len) => Reply::Read(&self.read[..len]),
        })
    }

    /// The run the last fetch brought.
    pub fn landed(&self) -> &[u8] {
        &self.landed.bytes
    }

    /// Write `data` to sector `sector` of `medium`, answered once the front-end
    /// has written it.
    pub fn write(&mut self, id: u16, sector: u64, data: &[u8; SECTOR]) -> Result<(), MediaError> {
        let medium = self.medium(id).ok_or(MediaError::Failed)?;
        if sector >= medium.form.units() {
            return Err(MediaError::OutOfRange);
        }
        if medium.form.read_only() {
            return Err(MediaError::WriteProtected);
        }
        let request = self.request(&Event::Store {
            request: self.next_request,
            medium: id,
            sector,
            data,
        })?;
        let answer = self.await_answer(
            request,
            Err(MediaError::Failed),
            |command, _| match command {
                Command::Stored { answer, .. } => Some(answer),
                _ => None,
            },
        );
        let held = self.landed.at(id, sector);
        if answer.is_ok() {
            self.cache.written(id, sector, data);
            if let Some(at) = held {
                self.landed.bytes[at..at + SECTOR].copy_from_slice(data);
            }
        } else {
            self.cache.forget(id, sector);
            if held.is_some() {
                self.landed.bytes.clear();
            }
        }
        answer
    }

    /// Send `event`, a request, and answer its number.
    fn request(&mut self, event: &Event<'_>) -> Result<u32, MediaError> {
        if self.ended.is_some() {
            return Err(MediaError::Failed);
        }
        let request = self.next_request;
        self.next_request = request.wrapping_add(1);
        self.send(event);
        self.flush();
        if self.ended.is_some() {
            return Err(MediaError::Failed);
        }
        Ok(request)
    }

    /// Read commands until the answer to `request` comes, keeping every other,
    /// and answer what `answer` makes of it; `failed` once the session has
    /// ended, or the answer is not one to what was asked.
    fn await_answer<T>(
        &mut self,
        request: u32,
        failed: T,
        mut answer: impl FnMut(Command<'_>, &mut Lands<'_>) -> Option<T>,
    ) -> T {
        let began = self.link.now();
        let result = loop {
            let Self {
                link,
                landed,
                read,
                deferred,
                deferred_count,
                deferred_typed,
                released,
                fill,
                band,
                ..
            } = self;
            let mut lands = Lands { run: landed, read };
            let got = link.recv(None, |payload| -> Result<Option<T>, &'static str> {
                let command = Command::decode(payload).map_err(|_| "a malformed command")?;
                match command {
                    Command::Fetched { request: r, .. }
                    | Command::Stored { request: r, .. }
                    | Command::Recalled { request: r, .. }
                    | Command::Foldered { request: r, .. } => {
                        if r != request {
                            return Err("an answer to nothing asked");
                        }
                        answer(command, &mut lands)
                            .map(Some)
                            .ok_or("an answer of the wrong kind or size")
                    }
                    Command::Released { slot } => {
                        *released |= 1u32
                            .checked_shl(u32::from(slot))
                            .ok_or("a slot past any layout")?;
                        Ok(None)
                    }
                    Command::Fill { frames } => {
                        *fill = Some(frames);
                        Ok(None)
                    }
                    Command::Pressure(reported) => {
                        *band = Some(reported);
                        Ok(None)
                    }
                    Command::Type(codes) => {
                        let typed = *deferred_typed + codes.len();
                        if typed > TYPE_WINDOW {
                            return Err("more to type than the window holds");
                        }
                        keep(deferred, payload)?;
                        *deferred_typed = typed;
                        Ok(None)
                    }
                    _ => {
                        if *deferred_count == INPUT_WINDOW {
                            return Err("more commands than the window allows");
                        }
                        keep(deferred, payload)?;
                        *deferred_count += 1;
                        Ok(None)
                    }
                }
            });
            match got {
                Ok(Waited::Frame(Ok(Some(result)))) => break result,
                Ok(Waited::Frame(Ok(None)) | Waited::Nothing) => {}
                Ok(Waited::Frame(Err(violation))) => {
                    self.end(End::Violation(violation));
                    break failed;
                }
                Ok(Waited::Closed) => {
                    self.end(End::Closed);
                    break failed;
                }
                Err(error) => {
                    self.end(End::Failed(error));
                    break failed;
                }
            }
        };
        self.waited = self
            .waited
            .saturating_add(self.link.now().saturating_sub(began));
        result
    }
}

/// A medium the front-end holds, as the machine's drive reaches it.
pub struct RemoteDisk<L> {
    port: Rc<RefCell<Port<L>>>,
    medium: u16,
    sectors: u64,
    read_only: bool,
}

impl<L: Link> RemoteDisk<L> {
    /// The disk reaching `medium`, of `sectors`, through `port`.
    pub fn new(port: Rc<RefCell<Port<L>>>, medium: u16, sectors: u64, read_only: bool) -> Self {
        Self {
            port,
            medium,
            sectors,
            read_only,
        }
    }
}

impl<L: Link> BlockDevice for RemoteDisk<L> {
    fn sectors(&self) -> u64 {
        self.sectors
    }

    fn read_only(&self) -> bool {
        self.read_only
    }

    fn read(&mut self, sector: u64, out: &mut [u8; SECTOR]) -> Result<(), MediaError> {
        // The run loop never holds the port while the machine runs.
        let mut port = self.port.try_borrow_mut().map_err(|_| MediaError::Failed)?;
        port.read(self.medium, sector, out)
    }

    fn write(&mut self, sector: u64, data: &[u8; SECTOR]) -> Result<(), MediaError> {
        let mut port = self.port.try_borrow_mut().map_err(|_| MediaError::Failed)?;
        port.write(self.medium, sector, data)
    }
}
