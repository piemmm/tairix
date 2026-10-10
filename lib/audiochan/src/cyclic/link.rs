//! The production channel: a DMA client opened on the device's link, and this
//! process's mapping of the buffer its controller carves.

use tairix_abi::driver::dmaengine::{CyclicParams, DmaDirection, WaitReport};
use tairix_abi::time::Time64;
use tairix_abi::DriverError;
use tairix_linkclient::{DmaClient, RtLinkCall};
use tairix_rt::shm::MappedGrant;

use super::DmaPort;

/// A DMA channel playing into a device's FIFO.
pub struct LinkDma {
    client: DmaClient<RtLinkCall>,
    /// The FIFO's CPU address, inside the device's own register window.
    fifo: u64,
    buffer: Option<MappedGrant>,
}

impl LinkDma {
    /// The channel `client` opened, playing into the FIFO at `fifo`.
    #[must_use]
    pub const fn new(client: DmaClient<RtLinkCall>, fifo: u64) -> Self {
        Self {
            client,
            fifo,
            buffer: None,
        }
    }
}

impl DmaPort for LinkDma {
    fn prepare(&mut self, period_bytes: u32, periods: u32) -> Result<(), DriverError> {
        let params = CyclicParams {
            fifo: self.fifo,
            direction: DmaDirection::MemoryToDevice,
            period_bytes,
            periods,
        };
        let bytes = params.buffer_bytes().map_err(DriverError::from_errno)?;
        let least = usize::try_from(bytes).map_err(|_| DriverError::OutOfRange)?;
        // The controller frees the buffer this one replaces, so the old
        // mapping goes first.
        self.buffer = None;
        let grant = self
            .client
            .prepare(&params)
            .map_err(DriverError::from_errno)?;
        self.buffer = Some(
            MappedGrant::map(grant.grantor, grant.grant, least).map_err(DriverError::from_errno)?,
        );
        Ok(())
    }

    fn buffer(&mut self) -> &mut [u8] {
        self.buffer.as_mut().map_or(&mut [], MappedGrant::bytes_mut)
    }

    fn start(&mut self) -> Result<(), DriverError> {
        self.client.start().map_err(DriverError::from_errno)
    }

    fn stop(&mut self) -> Result<(), DriverError> {
        self.client.stop().map_err(DriverError::from_errno)
    }

    fn wait(&mut self, after: u64) -> Result<WaitReport, DriverError> {
        self.client.wait(after).map_err(DriverError::from_errno)
    }

    fn post_wait(&mut self, after: u64, deadline_ns: u64) -> Result<(), DriverError> {
        self.client
            .post_wait(after, deadline_ns)
            .map_err(DriverError::from_errno)
    }

    fn reap_wait(&mut self) -> Result<Option<WaitReport>, DriverError> {
        self.client.reap_wait().map_err(DriverError::from_errno)
    }

    fn is_waiting(&self) -> bool {
        self.client.is_waiting()
    }

    fn now(&self) -> Time64 {
        Time64::from_nanos(tairix_rt::clock_get())
    }
}
