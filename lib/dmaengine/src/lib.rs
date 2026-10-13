//! The `dmaengine-v1` controller side (`plans/SOUND.md` SND5, SND19a): what
//! every DMA controller driver serves around its
//! [`DmaEngine`](tairix_abi::driver::dmaengine::DmaEngine).
//!
//! A control block holds bus addresses and nothing between a controller and
//! RAM checks them, so the controller's driver is the only process that
//! writes one. [`Controller`] is the endpoint its consumers call: it believes
//! a request line or a FIFO only once the kernel attests the caller holds it,
//! carves every buffer itself, and answers waits by which period a channel
//! has reached. It is written over the class traits alone and is
//! host-testable; `serve` is the freestanding bring-up and loop a driver hands
//! its engine to.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod controller;

#[cfg(test)]
mod controller_tests;

pub use controller::{split_windows, Buffer, Controller, ControllerHost, Record};

#[cfg(target_os = "none")]
mod serve;
#[cfg(target_os = "none")]
pub use serve::{serve, Node};

use tairix_abi::driver::dmaengine::DMA_MAX_CHANNELS;
#[cfg(any(test, target_os = "none"))]
use tairix_log::{Field, FieldValue};

/// The channels every per-channel table holds.
const CHANNEL_SLOTS: usize = DMA_MAX_CHANNELS as usize;

/// The fields a serving controller's ready record carries for `usable`, a
/// mask over its node's channels: how many it serves, and which.
#[cfg(any(test, target_os = "none"))]
fn ready_fields(usable: u64, hex: &mut [u8; 16]) -> [Field<'_>; 2] {
    [
        Field {
            key: "channels",
            value: FieldValue::UnsignedInt(u64::from(usable.count_ones())),
        },
        Field {
            key: "channel_mask_hex",
            value: FieldValue::Str(tairix_util::fmt::format_hex_u64(usable, hex)),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ready_record_counts_the_channels_served_and_names_them_by_mask() {
        let mut hex = [0u8; 16];
        let [count, mask] = ready_fields(0x35_35, &mut hex);
        assert_eq!(count.key, "channels");
        assert_eq!(count.value, FieldValue::UnsignedInt(8));
        assert_eq!(mask.key, "channel_mask_hex");
        assert_eq!(mask.value, FieldValue::Str("0000000000003535"));
    }
}
