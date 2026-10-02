//! What the decoders' tests share: report-descriptor item builders and a
//! sink that records what it is given.

use alloc::vec::Vec;

use tairix_abi::input::{KeyInput, PointerInput};
use tairix_abi::touch::TouchFrame;
use tairix_abi::DriverError;

use crate::SeatSink;

/// Report-descriptor items, one builder per item the tests write.
pub(crate) mod items {
    use alloc::vec;
    use alloc::vec::Vec;

    /// One short item: `prefix` (tag and type) with `data` little-endian.
    pub(crate) fn item(prefix: u8, data: &[u8]) -> Vec<u8> {
        let size = match data.len() {
            0 => 0,
            1 => 1,
            2 => 2,
            _ => 3,
        };
        let mut bytes = vec![prefix | size];
        bytes.extend_from_slice(data);
        bytes
    }

    pub(crate) fn usage_page(page: u8) -> Vec<u8> {
        item(0x04, &[page])
    }
    pub(crate) fn usage(id: u8) -> Vec<u8> {
        item(0x08, &[id])
    }
    pub(crate) fn usage_min(id: u8) -> Vec<u8> {
        item(0x18, &[id])
    }
    pub(crate) fn usage_max(id: u8) -> Vec<u8> {
        item(0x28, &[id])
    }
    pub(crate) fn logical_min(value: i8) -> Vec<u8> {
        item(0x14, &value.to_le_bytes())
    }
    pub(crate) fn logical_max(value: u8) -> Vec<u8> {
        item(0x24, &[value])
    }
    pub(crate) fn report_size(bits: u8) -> Vec<u8> {
        item(0x74, &[bits])
    }
    pub(crate) fn report_count(count: u8) -> Vec<u8> {
        item(0x94, &[count])
    }
    pub(crate) fn report_id(id: u8) -> Vec<u8> {
        item(0x84, &[id])
    }
    pub(crate) fn collection(kind: u8) -> Vec<u8> {
        item(0xA0, &[kind])
    }
    pub(crate) fn end_collection() -> Vec<u8> {
        item(0xC0, &[])
    }
    pub(crate) fn input(flags: u8) -> Vec<u8> {
        item(0x80, &[flags])
    }
    pub(crate) fn output(flags: u8) -> Vec<u8> {
        item(0x90, &[flags])
    }
    pub(crate) fn feature(flags: u8) -> Vec<u8> {
        item(0xB0, &[flags])
    }

    pub(crate) fn join(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    pub(crate) const DATA_VAR: u8 = 0x02;
    pub(crate) const DATA_VAR_REL: u8 = 0x06;
    pub(crate) const DATA_ARRAY: u8 = 0x00;
    pub(crate) const CONSTANT: u8 = 0x01;

    pub(crate) fn unit(code: u8) -> Vec<u8> {
        item(0x64, &[code])
    }
    pub(crate) fn unit_exponent(exponent: u8) -> Vec<u8> {
        item(0x54, &[exponent])
    }
    pub(crate) fn physical_max(value: u16) -> Vec<u8> {
        item(0x44, &value.to_le_bytes())
    }
    pub(crate) fn logical_max16(value: u16) -> Vec<u8> {
        item(0x24, &value.to_le_bytes())
    }
}

/// Every record a decoder delivered, in order.
#[derive(Debug, Default)]
pub(crate) struct Recorder {
    pub(crate) keys: Vec<KeyInput>,
    pub(crate) pointer: Vec<PointerInput>,
    pub(crate) touch: Vec<TouchFrame>,
}

impl SeatSink for Recorder {
    fn key(&mut self, record: &KeyInput) -> Result<(), DriverError> {
        self.keys.push(*record);
        Ok(())
    }

    fn pointer(&mut self, record: &PointerInput) -> Result<(), DriverError> {
        self.pointer.push(*record);
        Ok(())
    }

    fn touch(&mut self, frame: &TouchFrame) -> Result<(), DriverError> {
        self.touch.push(*frame);
        Ok(())
    }
}
