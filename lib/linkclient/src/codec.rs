//! [`CodecClient`]: one codec, reached through the link that names it.

use tairix_abi::driver::audio::Rate;
use tairix_abi::driver::codec::{
    decode_describe_reply, decode_done_reply, decode_gain_reply, refusal, CodecFacts, CodecRequest,
    DaiLink, CODEC_MAX_REPLY, CODEC_MAX_REQUEST,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::{DriverError, Errno};

use crate::call::ask;
use crate::LinkCall;

/// The codec a digital audio interface's node names, as its driver serves it.
///
/// Its refusals are the codec driver's own, decoded by `codec-v1`'s
/// [`refusal`], so a caller tells a codec with no gain from one that failed.
pub struct CodecClient<C: LinkCall> {
    call: C,
    link: LinkRequest,
    dai: DaiLink,
}

impl<C: LinkCall> CodecClient<C> {
    /// The codec `link` names, the codec link the caller's node holds.
    ///
    /// # Errors
    ///
    /// [`Errno::BadMagic`] for a link whose selector is not a [`DaiLink`].
    pub fn new(call: C, link: LinkRequest) -> Result<Self, Errno> {
        let dai = DaiLink::from_cells(link.selector())?;
        Ok(Self { call, link, dai })
    }

    /// The framing the link states, and which side drives each clock.
    #[must_use]
    pub const fn dai(&self) -> &DaiLink {
        &self.dai
    }

    /// What the codec accepts.
    ///
    /// # Errors
    ///
    /// The codec's or the transport's refusal.
    pub fn describe(&mut self) -> Result<CodecFacts, DriverError> {
        self.ask(&CodecRequest::Describe(self.link), decode_describe_reply)
    }

    /// Take the codec, and set it up for `rate` and `width`-bit samples in
    /// the link's framing.
    ///
    /// # Errors
    ///
    /// [`DriverError::Busy`] for a codec another live process holds,
    /// [`DriverError::Unsupported`] for a rate, width or framing it does not
    /// take, or another refusal.
    pub fn configure(&mut self, rate: Rate, width: u8) -> Result<(), DriverError> {
        let request = CodecRequest::Configure {
            link: self.link,
            rate,
            width,
        };
        self.ask(&request, decode_done_reply)
    }

    /// Set the gain at or above `millibel` and the mute, answering the gain
    /// the codec set.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotImplemented`] for a codec with no gain, or another
    /// refusal.
    pub fn gain(&mut self, millibel: i32, mute: bool) -> Result<i32, DriverError> {
        let request = CodecRequest::Gain {
            link: self.link,
            millibel,
            mute,
        };
        self.ask(&request, decode_gain_reply)
    }

    /// Bring the codec's output up.
    ///
    /// # Errors
    ///
    /// The codec's or the transport's refusal.
    pub fn start(&mut self) -> Result<(), DriverError> {
        self.ask(&CodecRequest::Start(self.link), decode_done_reply)
    }

    /// Take the codec's output down.
    ///
    /// # Errors
    ///
    /// The codec's or the transport's refusal.
    pub fn stop(&mut self) -> Result<(), DriverError> {
        self.ask(&CodecRequest::Stop(self.link), decode_done_reply)
    }

    fn ask<T>(
        &mut self,
        request: &CodecRequest,
        decode: impl FnOnce(&[u8]) -> Result<T, Errno>,
    ) -> Result<T, DriverError> {
        ask::<_, _, CODEC_MAX_REQUEST, CODEC_MAX_REPLY>(
            &mut self.call,
            |frame| request.encode(frame),
            decode,
        )
        .map_err(refusal)
    }
}
