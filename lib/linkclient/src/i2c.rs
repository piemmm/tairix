//! [`I2cClient`]: a channel on an I²C bus, reached through the link that names
//! it; [`I2cTarget`]: one that reaches a single target, the port a
//! register-addressed chip is driven through.

use tairix_abi::driver::i2c::{
    decode_close_reply, decode_open_reply, decode_transfer_reply, BusFacts, I2cAddress, I2cFault,
    I2cPort, I2cReach, I2cRequest, I2cTransaction, I2C_BUFFER_LEN, I2C_MAX_REPLY, I2C_MAX_REQUEST,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::{DriverError, Errno, ProcId};
use tairix_rt::shm::MappedGrant;

use crate::call::ask;
use crate::LinkCall;

/// A channel's buffer, as its client has it mapped.
pub trait ChannelBuffer {
    /// The mapped bytes.
    fn bytes_mut(&mut self) -> &mut [u8];
}

impl ChannelBuffer for MappedGrant {
    fn bytes_mut(&mut self) -> &mut [u8] {
        Self::bytes_mut(self)
    }
}

fn request<C: LinkCall, T>(
    call: &mut C,
    request: &I2cRequest,
    decode: impl FnOnce(&[u8]) -> Result<T, Errno>,
) -> Result<T, Errno> {
    ask::<_, _, I2C_MAX_REQUEST, I2C_MAX_REPLY>(call, |frame| request.encode(frame), decode)
}

/// A channel on an I²C bus: what the bus states about itself, and the buffer
/// its transactions move their bytes through.
///
/// The channel is the opening instance's until [`close`](Self::close) or its
/// end, when the controller drops it and its buffer.
pub struct I2cClient<C: LinkCall, B: ChannelBuffer> {
    call: C,
    link: LinkRequest,
    facts: BusFacts,
    buffer: B,
}

impl<C: LinkCall, B: ChannelBuffer> I2cClient<C, B> {
    /// Open the channel `link` names, the I²C link the caller's node holds,
    /// and map the buffer it is granted with `map`, given the grantor, the
    /// grant and the least length the mapping must have: [`MappedGrant::map`]
    /// in a driver.
    ///
    /// # Errors
    ///
    /// The controller's, the transport's or the mapping's refusal, or
    /// [`Errno::LengthOutOfRange`] for a mapping shorter than
    /// [`I2C_BUFFER_LEN`]. A channel whose buffer could not be mapped is
    /// closed again.
    pub fn open(
        mut call: C,
        link: LinkRequest,
        map: impl FnOnce(ProcId, u64, usize) -> Result<B, Errno>,
    ) -> Result<Self, Errno> {
        let channel = request(&mut call, &I2cRequest::Open(link), decode_open_reply)?;
        let mapped = map(channel.buffer.grantor, channel.buffer.grant, I2C_BUFFER_LEN).and_then(
            |mut buffer| {
                if buffer.bytes_mut().len() < I2C_BUFFER_LEN {
                    return Err(Errno::LengthOutOfRange);
                }
                Ok(buffer)
            },
        );
        match mapped {
            Ok(buffer) => Ok(Self {
                call,
                link,
                facts: channel.facts,
                buffer,
            }),
            Err(reason) => {
                // The refusal to report is the mapping's; a close the
                // controller also refuses changes nothing for the caller.
                let _ = request(&mut call, &I2cRequest::Close(link), decode_close_reply);
                Err(reason)
            }
        }
    }

    /// What the bus states about itself.
    #[must_use]
    pub const fn facts(&self) -> BusFacts {
        self.facts
    }

    /// The channel's buffer: write bytes are staged in it before a
    /// transaction, read bytes found in it after.
    pub fn buffer(&mut self) -> &mut [u8] {
        self.buffer.bytes_mut()
    }

    /// Run `transaction` over the buffer, answering how it ended on the bus.
    ///
    /// # Errors
    ///
    /// The controller's or the transport's refusal: the bus was not touched,
    /// or its controller could not run the transaction.
    pub fn run(&mut self, transaction: &I2cTransaction) -> Result<Result<(), I2cFault>, Errno> {
        let transfer = I2cRequest::Transfer {
            link: self.link,
            transaction: *transaction,
        };
        request(&mut self.call, &transfer, decode_transfer_reply)
    }

    /// Close the channel; the controller drops its buffer.
    ///
    /// # Errors
    ///
    /// The controller's or the transport's refusal.
    pub fn close(mut self) -> Result<(), Errno> {
        request(
            &mut self.call,
            &I2cRequest::Close(self.link),
            decode_close_reply,
        )
    }
}

/// A channel to one target, on a bus that can follow a write with a read.
pub struct I2cTarget<C: LinkCall, B: ChannelBuffer> {
    channel: I2cClient<C, B>,
    address: I2cAddress,
}

impl<C: LinkCall, B: ChannelBuffer> I2cTarget<C, B> {
    /// Open the channel `link` names, which must reach one target, mapping
    /// its buffer as [`I2cClient::open`] does.
    ///
    /// # Errors
    ///
    /// * [`Errno::OutOfRange`] for a link that does not name one target.
    /// * [`Errno::NotSupported`] for a bus that cannot issue the repeated
    ///   START between a register write and its read; the channel is closed.
    /// * Any refusal [`I2cClient::open`] reports.
    pub fn open(
        call: C,
        link: LinkRequest,
        map: impl FnOnce(ProcId, u64, usize) -> Result<B, Errno>,
    ) -> Result<Self, Errno> {
        let I2cReach::Target(address) = I2cReach::of(&link)? else {
            return Err(Errno::OutOfRange);
        };
        let channel = I2cClient::open(call, link, map)?;
        let write_then_read = I2cTransaction::write_then_read(address, 1, 1)?;
        if !channel.facts().repeated_start.admits(&write_then_read) {
            // The refusal to report is the bus's shape; a close the
            // controller also refuses changes nothing for the caller.
            let _ = channel.close();
            return Err(Errno::NotSupported);
        }
        Ok(Self { channel, address })
    }

    /// What the bus states about itself.
    #[must_use]
    pub const fn facts(&self) -> BusFacts {
        self.channel.facts()
    }

    /// Close the channel; the controller drops its buffer.
    ///
    /// # Errors
    ///
    /// The controller's or the transport's refusal.
    pub fn close(self) -> Result<(), Errno> {
        self.channel.close()
    }
}

impl<C: LinkCall, B: ChannelBuffer> I2cPort for I2cTarget<C, B> {
    fn transfer(&mut self, write: &[u8], read: &mut [u8]) -> Result<(), DriverError> {
        let transaction = I2cTransaction::write_then_read(self.address, write.len(), read.len())
            .map_err(DriverError::from_errno)?;
        let staged = self.channel.buffer();
        staged
            .get_mut(..write.len())
            .ok_or(DriverError::LengthOutOfRange)?
            .copy_from_slice(write);
        self.channel
            .run(&transaction)
            .map_err(DriverError::from_errno)?
            .map_err(I2cFault::as_driver_error)?;
        let received = self
            .channel
            .buffer()
            .get(write.len()..write.len() + read.len())
            .ok_or(DriverError::LengthOutOfRange)?;
        read.copy_from_slice(received);
        Ok(())
    }
}

#[cfg(test)]
#[path = "i2c_tests.rs"]
mod tests;
