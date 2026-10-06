//! How a unit's family reaches its registers: the production window, or a
//! register-level model in the host tests.

use tairix_abi::RegisterWindow;

use crate::IommuError;

/// A unit's register set.
pub trait Registers: Send {
    /// Read the 32-bit register at `offset`.
    ///
    /// # Errors
    ///
    /// [`IommuError::Hardware`] for an offset outside the register set.
    fn read32(&self, offset: usize) -> Result<u32, IommuError>;

    /// Write the 32-bit register at `offset`.
    ///
    /// # Errors
    ///
    /// As [`Self::read32`].
    fn write32(&self, offset: usize, value: u32) -> Result<(), IommuError>;

    /// Read the 64-bit register at `offset` as one access.
    ///
    /// # Errors
    ///
    /// As [`Self::read32`].
    fn read64(&self, offset: usize) -> Result<u64, IommuError>;

    /// Write the 64-bit register at `offset` as one access.
    ///
    /// # Errors
    ///
    /// As [`Self::read32`].
    fn write64(&self, offset: usize, value: u64) -> Result<(), IommuError>;

    /// Bytes of register set the window reaches.
    fn window_len(&self) -> usize;
}

impl Registers for RegisterWindow {
    fn read32(&self, offset: usize) -> Result<u32, IommuError> {
        self.read_u32(offset).map_err(|_| IommuError::Hardware)
    }

    fn write32(&self, offset: usize, value: u32) -> Result<(), IommuError> {
        self.write_u32(offset, value)
            .map_err(|_| IommuError::Hardware)
    }

    fn read64(&self, offset: usize) -> Result<u64, IommuError> {
        self.read_u64(offset).map_err(|_| IommuError::Hardware)
    }

    fn write64(&self, offset: usize, value: u64) -> Result<(), IommuError> {
        self.write_u64(offset, value)
            .map_err(|_| IommuError::Hardware)
    }

    fn window_len(&self) -> usize {
        self.len()
    }
}
