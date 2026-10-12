//! What a display sink states about itself (`plans/SOUND.md` `SND19j`).
//!
//! [`Edid::parse`] reads an EDID — the base block and its CTA-861 and
//! `DisplayID` extensions — into the sink's identity, the timings it takes, the
//! audio it plays and its HDMI capabilities; [`block_count`] says how many
//! blocks to read before it. [`Eld::parse`] reads the same audio from an HD
//! Audio codec's ELD. [`Avi`] and [`hdmi_vendor`] pack the `InfoFrames` a
//! source sends the sink back.
//!
//! Every byte is untrusted device input. Each block's checksum is checked,
//! every length and offset is held to the block it lies in, and what is
//! malformed is refused — the base block — or skipped, never read past. What
//! a sink lists is kept up to fixed bounds, the first of each kept.
//!
//! References: VESA E-EDID 1.4 and DMT; CTA-861-H; `DisplayID` 1.3 and 2.0;
//! HDMI 1.4b and 2.1; Intel HD Audio 1.0a §7.3.3.34 (the ELD).

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[cfg(any(test, feature = "test-support"))]
extern crate alloc;

mod allocation;
mod audio;
mod cta;
mod displayid;
mod edid;
mod eld;
mod infoframe;
#[cfg(any(test, feature = "test-support"))]
pub mod synth;
mod tables;
mod timing;

#[cfg(test)]
mod tests;

pub use allocation::{allocate, widest_layout, Allocation, SLOTS};
pub use audio::{
    Audio, AudioFormat, SampleRates, SampleSizes, ShortAudioDescriptor, Speakers,
    MAX_AUDIO_DESCRIPTORS,
};
pub use cta::{hdmi_video_code, video_code};
pub use edid::{
    block_count, Edid, EdidError, Hdmi, HdmiForum, Mode, Product, VideoCode, BLOCK_LEN, MAX_BLOCKS,
    MAX_MODES, NAME_LEN,
};
pub use eld::{Connection, Eld, EldError, ELD_MAX_LEN, ELD_NAME_LEN};
pub use infoframe::{hdmi_vendor, AudioInfo, Avi, Packed, Quantization, INFOFRAME_MAX};
pub use tairix_abi::driver::display::{Axis, Polarity, Scan, Timing};
pub use timing::{Aspect, Size};
