//! The display loop's end of the GPU's desk: each turn it takes the answers
//! the device gave the desk's 3D commands back to the desk, then carries out
//! the orders waiting, putting back the first the device has no room for
//! until a completion frees it.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::driver::MemoryMapper;
use tairix_abi::DriverError;
use tairix_display::{BufferMemory, DeviceBuffer};
use tairix_drvrt::Recorder;
use tairix_rt::sync::Mutex;
use tairix_virtio::{Part, SharedMemory, Transport};

use super::desk::{Answer, Desk, Order, Payload, PAYLOAD_MAX};
use crate::three_d::{Blob, Context, Done, Stream};
use crate::{ConsoleSource, Record, VirtioGpu};

/// The most nudges one turn drains: only the driver's own threads may send
/// them, and a turn serves every order however many brought it.
const NUDGES_PER_TURN: usize = 64;

/// The display loop's side of a desk.
pub(crate) trait Counter: Send + Sync {
    fn begin_turn(&self);
    fn take(&self) -> Option<(u64, Order)>;
    fn put_back(&self, tag: u64, order: Order);
    fn answer(&self, tag: u64, answer: Answer);
}

impl<T: Send> Counter for Desk<T> {
    fn begin_turn(&self) {
        Desk::begin_turn(self);
    }

    fn take(&self) -> Option<(u64, Order)> {
        Desk::take(self)
    }

    fn put_back(&self, tag: u64, order: Order) {
        Desk::put_back(self, tag, order);
    }

    fn answer(&self, tag: u64, answer: Answer) {
        Desk::answer(self, tag, answer);
    }
}

/// What an order with the device holds on the display loop's side until it
/// is answered.
enum Lent {
    Nothing,
    Context(Context),
    Blob(Blob),
    Map(Blob, Part),
    Unmap(Blob, Part),
    Slot(usize),
}

/// The display loop's end of a desk, where the device takes Venus work.
pub(crate) struct DeskLink<'h, R> {
    counter: Arc<dyn Counter>,
    shared: Arc<Mutex<SharedMemory>>,
    mapper: &'h dyn MemoryMapper,
    /// The port the desk's nudges arrive on, drained each turn.
    port: Option<u64>,
    /// Device memory each submission's commands lie in, a slot apiece.
    arena: R,
    free: Vec<usize>,
    lent: Vec<(u64, Lent)>,
}

impl<'h, T, M, C, L> VirtioGpu<'h, T, M, C, L>
where
    T: Transport,
    M: BufferMemory,
    M::Region: DeviceBuffer,
    C: ConsoleSource,
    L: Recorder<Record>,
{
    /// Serve `counter`'s orders from here on: blobs are placed in `shared`
    /// and mapped into the driver through `mapper`, and the desk's nudges
    /// arrive on `port`.
    ///
    /// # Errors
    ///
    /// [`DriverError::Unsupported`] where the device takes no Venus work,
    /// [`DriverError::Busy`] where a desk is already served, and
    /// [`DriverError::OutOfMemory`] where the memory a submission's commands
    /// lie in cannot be had.
    pub(crate) fn attach_desk(
        &mut self,
        counter: Arc<dyn Counter>,
        shared: Arc<Mutex<SharedMemory>>,
        mapper: &'h dyn MemoryMapper,
        port: Option<u64>,
    ) -> Result<(), DriverError> {
        let room = self
            .three_d
            .as_ref()
            .ok_or(DriverError::Unsupported)?
            .commands_room();
        if self.desk.is_some() {
            return Err(DriverError::Busy);
        }
        let arena = self.memory.create(room * PAYLOAD_MAX)?;
        let mut free = Vec::new();
        free.try_reserve_exact(room)
            .map_err(|_| DriverError::OutOfMemory)?;
        free.extend(0..room);
        let mut lent = Vec::new();
        lent.try_reserve_exact(room)
            .map_err(|_| DriverError::OutOfMemory)?;
        self.desk = Some(DeskLink {
            counter,
            shared,
            mapper,
            port,
            arena,
            free,
            lent,
        });
        Ok(())
    }

    /// The desk's turn: its nudges drained, what the device answered handed
    /// back, and the orders waiting carried out.
    pub(crate) fn serve_desk(&mut self) {
        let Some(mut link) = self.desk.take() else {
            return;
        };
        link.drain_nudges();
        link.counter.begin_turn();
        while let Some(completion) = self.completion() {
            let Some(at) = link.lent.iter().position(|(tag, _)| *tag == completion.tag) else {
                continue;
            };
            let (tag, lent) = link.lent.swap_remove(at);
            let free = match &lent {
                Lent::Map(blob, _) | Lent::Unmap(blob, _) => self.blob_unmapped(*blob),
                _ => false,
            };
            let answer = link.answered(lent, completion.outcome, free);
            link.counter.answer(tag, answer);
        }
        while let Some((tag, order)) = link.counter.take() {
            if let Some(order) = self.carry_out(&mut link, tag, order) {
                link.counter.put_back(tag, order);
                break;
            }
        }
        self.desk = Some(link);
    }

    /// Carry out `order`, or hand it back where the device has no room for
    /// it yet.
    fn carry_out(
        &mut self,
        link: &mut DeskLink<'_, M::Region>,
        tag: u64,
        order: Order,
    ) -> Option<Order> {
        let sent = match order {
            Order::MakeContext => match self.create_context(b"tairix-gpu", tag) {
                Ok(context) => Ok(Lent::Context(context)),
                Err(DriverError::Busy) => return Some(Order::MakeContext),
                Err(refused) => Err((refused, None)),
            },
            Order::EndContext(context) => match self.destroy_context(context, tag) {
                Ok(()) => Ok(Lent::Nothing),
                Err(DriverError::Busy) => return Some(Order::EndContext(context)),
                Err(refused) => Err((refused, None)),
            },
            Order::MakeBlob(context, spec) => match self.create_blob(context, spec, tag) {
                Ok(blob) => Ok(Lent::Blob(blob)),
                Err(DriverError::Busy) => return Some(Order::MakeBlob(context, spec)),
                Err(refused) => Err((refused, None)),
            },
            Order::EndBlob(blob) => match self.destroy_blob(blob, tag) {
                Ok(()) => Ok(Lent::Nothing),
                Err(DriverError::Busy) => return Some(Order::EndBlob(blob)),
                Err(refused) => Err((refused, None)),
            },
            Order::MapBlob(blob, part) => match self.map_blob(blob, part.offset(), tag) {
                Ok(()) => Ok(Lent::Map(blob, part)),
                Err(DriverError::Busy) => return Some(Order::MapBlob(blob, part)),
                Err(refused) => Err((refused, Some(part))),
            },
            Order::UnmapBlob(blob, mut part) => {
                // Out of the driver first, so no CPU reaches what the device
                // is about to take back; unmapping again is a no-op.
                let unmapped = link.shared.lock().unmap(&mut part, link.mapper);
                match unmapped {
                    Err(refused) => Err((refused.into(), self.blob_unmapped(blob).then_some(part))),
                    Ok(()) => match self.unmap_blob(blob, tag) {
                        Ok(()) => Ok(Lent::Unmap(blob, part)),
                        Err(DriverError::Busy) => return Some(Order::UnmapBlob(blob, part)),
                        Err(refused) => Err((refused, self.blob_unmapped(blob).then_some(part))),
                    },
                }
            }
            Order::Submit {
                context,
                ring,
                payload,
            } => {
                let Some(slot) = link.free.pop() else {
                    return Some(Order::Submit {
                        context,
                        ring,
                        payload,
                    });
                };
                let stream = link.place(slot, &payload);
                match self.submit_3d(context, ring, stream, tag) {
                    Ok(()) => Ok(Lent::Slot(slot)),
                    Err(DriverError::Busy) => {
                        link.free.push(slot);
                        return Some(Order::Submit {
                            context,
                            ring,
                            payload,
                        });
                    }
                    Err(refused) => {
                        link.free.push(slot);
                        Err((refused, None))
                    }
                }
            }
        };
        match sent {
            // Room was reserved for every 3D command the queue leaves room for.
            Ok(lent) => link.lent.push((tag, lent)),
            Err((refused, part)) => link.counter.answer(tag, Answer::Refused(refused, part)),
        }
        None
    }
}

impl<R: DeviceBuffer> DeskLink<'_, R> {
    fn drain_nudges(&self) {
        let Some(port) = self.port else {
            return;
        };
        let mut nudge = [0u8; 1];
        let mut origin = [0u8; tairix_abi::ORIGIN_WIRE_LEN];
        for _ in 0..NUDGES_PER_TURN {
            if tairix_rt::ipc_recv(port, &mut nudge, &mut origin).is_err() {
                break;
            }
        }
    }

    /// Lay `payload` in `slot` of the arena: the stream a submission carries.
    fn place(&mut self, slot: usize, payload: &Payload) -> Stream {
        let at = slot * PAYLOAD_MAX;
        self.arena.with_bytes_mut(|bytes| {
            if let Some(into) = bytes.get_mut(at..at + payload.len()) {
                into.copy_from_slice(payload);
            }
        });
        Stream {
            device_addr: self.arena.device_addr() + at as u64,
            len: u32::try_from(payload.len()).unwrap_or(0),
        }
    }

    /// What the device's `outcome` of an order holding `lent` answers; `free`
    /// where the device is known to map nothing into a part lent.
    fn answered(&mut self, lent: Lent, outcome: Result<Done, DriverError>, free: bool) -> Answer {
        match (lent, outcome) {
            (Lent::Slot(slot), outcome) => {
                self.free.push(slot);
                match outcome {
                    Ok(_) => Answer::Done,
                    Err(refused) => Answer::Refused(refused, None),
                }
            }
            (Lent::Map(_, mut part), Ok(Done::Mapped(caching))) => {
                // The device mapped it: a part the driver could not map too
                // comes back windowless, for its sender to unmap again.
                let _ = self.shared.lock().map(&mut part, self.mapper, caching);
                Answer::Mapped(part)
            }
            (Lent::Unmap(_, part), Ok(_)) => Answer::Unmapped(part),
            // A part goes back to its sender only where the device maps nothing
            // into it: one it may still map is let go unreleased, its pages
            // never handed out again.
            (Lent::Map(_, part) | Lent::Unmap(_, part), outcome) => Answer::Refused(
                outcome.err().unwrap_or(DriverError::DeviceFault),
                free.then_some(part),
            ),
            (Lent::Context(context), Ok(_)) => Answer::Context(context),
            (Lent::Blob(blob), Ok(_)) => Answer::Blob(blob),
            (Lent::Nothing, Ok(_)) => Answer::Done,
            (Lent::Context(_) | Lent::Blob(_) | Lent::Nothing, Err(refused)) => {
                Answer::Refused(refused, None)
            }
        }
    }
}
