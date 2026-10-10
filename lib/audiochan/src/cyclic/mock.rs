//! A DMA channel that plays its buffer a period at a time when a test says
//! so, and answers waits as the controller does: the scaffold every
//! cyclic-DMA audio driver's tests need, defined once.

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use core::num::NonZeroU32;

use tairix_abi::driver::dmaengine::{WaitEnd, WaitReport};
use tairix_abi::time::{Duration64, Time64};
use tairix_abi::DriverError;

use super::DmaPort;

/// The modelled channel.
#[derive(Default)]
pub struct Channel {
    /// The buffer, as the controller carved it.
    pub buffer: Vec<u8>,
    /// The period the buffer was prepared for, in bytes.
    pub period_bytes: u32,
    /// Periods the buffer holds.
    pub periods: u32,
    /// Whether the channel runs.
    pub running: bool,
    /// Bytes moved since the start.
    pub moved: u64,
    /// The posted wait's position, and its answer once given.
    posted: Option<(u64, Option<WaitReport>)>,
    /// Every 32-bit word the channel has sent, in order.
    pub sent: Vec<u32>,
    /// Starts and stops the channel was asked for.
    pub starts: u32,
    /// Stops the channel was asked for.
    pub stops: u32,
    /// The next boundary faults the channel instead.
    pub fault: bool,
}

fn report(end: WaitEnd, position: u64) -> WaitReport {
    WaitReport {
        end,
        position,
        serviced: Duration64::from_nanos(position),
    }
}

impl Channel {
    /// Play `count` periods, answering a posted wait at the first boundary
    /// past its position.
    ///
    /// # Panics
    ///
    /// When the channel is not running: a test advancing a stopped channel
    /// asserts something the hardware would not do.
    pub fn advance(&mut self, count: u32) {
        assert!(self.running, "advanced while stopped");
        for _ in 0..count {
            let period = u64::from(self.period_bytes);
            let slot = (self.moved / period) % u64::from(self.periods);
            let start = usize::try_from(slot * period).expect("a slot");
            let words = &self.buffer[start..start + self.period_bytes as usize];
            self.sent.extend(
                words
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|word| u32::from_le_bytes(*word)),
            );
            self.moved += period;
            if let Some((after, answer @ None)) = &mut self.posted {
                if self.moved > *after {
                    *answer = Some(if self.fault {
                        report(WaitEnd::Faulted(NonZeroU32::MIN), self.moved)
                    } else {
                        report(WaitEnd::Boundary, self.moved)
                    });
                }
            }
        }
    }

    /// Whether a wait is posted.
    #[must_use]
    pub const fn is_posted(&self) -> bool {
        self.posted.is_some()
    }
}

impl DmaPort for Channel {
    fn prepare(&mut self, period_bytes: u32, periods: u32) -> Result<(), DriverError> {
        assert!(!self.running, "prepared while running");
        self.buffer = vec![0xEE; (period_bytes * periods) as usize];
        self.period_bytes = period_bytes;
        self.periods = periods;
        Ok(())
    }

    fn buffer(&mut self) -> &mut [u8] {
        &mut self.buffer
    }

    fn start(&mut self) -> Result<(), DriverError> {
        assert!(!self.running, "started while running");
        self.running = true;
        self.moved = 0;
        self.starts += 1;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), DriverError> {
        self.running = false;
        self.stops += 1;
        if let Some((_, answer @ None)) = &mut self.posted {
            *answer = Some(report(WaitEnd::Stopped, self.moved));
        }
        Ok(())
    }

    fn wait(&mut self, after: u64) -> Result<WaitReport, DriverError> {
        assert!(self.posted.is_none(), "a blocking wait beside a posted one");
        while self.moved <= after {
            self.advance(1);
        }
        Ok(report(WaitEnd::Boundary, self.moved))
    }

    fn post_wait(&mut self, after: u64, deadline_ns: u64) -> Result<(), DriverError> {
        assert!(self.posted.is_none(), "two waits posted");
        assert!(deadline_ns > 0);
        self.posted = Some((after, None));
        Ok(())
    }

    fn reap_wait(&mut self) -> Result<Option<WaitReport>, DriverError> {
        match self.posted {
            Some((_, Some(answer))) => {
                self.posted = None;
                Ok(Some(answer))
            }
            _ => Ok(None),
        }
    }

    fn is_waiting(&self) -> bool {
        self.posted.is_some()
    }

    fn now(&self) -> Time64 {
        Time64::from_nanos(self.moved)
    }
}
